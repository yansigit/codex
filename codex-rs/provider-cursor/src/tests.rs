use super::*;
use codex_provider_api::Checkpoint;
use codex_provider_api::Credentials;
use codex_provider_api::Message as InputMessage;
use codex_provider_api::ToolDefinition;

fn request() -> Request {
    Request {
        namespace: Namespace {
            provider: "cursor".into(),
            account: "test-account".into(),
            model: "test-model".into(),
            thread: "test-thread".into(),
        },
        system: "Be helpful".into(),
        messages: vec![InputMessage {
            role: Role::User,
            content: vec![Content::Text {
                text: "Run a tool then answer".into(),
            }],
        }],
        tools: vec![ToolDefinition {
            name: "host_tool".into(),
            description: "A host tool".into(),
            parameters: json!({"type":"object","properties":{"count":{"type":"integer"}}}),
        }],
        parallel_tool_calls: false,
        max_output_tokens: None,
        reasoning_effort: None,
        output_schema: None,
        checkpoint: None,
        metadata: Value::Null,
    }
}

fn context() -> Context {
    Context {
        credentials: Credentials {
            access_token: "test-token".into(),
            account_id: "test-account".into(),
            metadata: Value::Null,
        },
        cancellation: CancellationToken::new(),
    }
}

async fn send_message(send: &mut h2::SendStream<Bytes>, message: wire::ServerMessage, end: bool) {
    let data = connect::encode(&message.encode_to_vec()).unwrap();
    // Split even the envelope header across DATA frames.
    send.send_data(data.slice(..2), false).unwrap();
    send.send_data(data.slice(2..), end).unwrap();
}

async fn receive_message(
    recv: &mut h2::RecvStream,
    decoder: &mut connect::Decoder,
) -> wire::ClientMessage {
    loop {
        if let Some(connect::Frame::Message(payload)) = decoder.next().unwrap() {
            return wire::ClientMessage::decode(payload).unwrap();
        }
        let chunk = recv.data().await.unwrap().unwrap();
        recv.flow_control().release_capacity(chunk.len()).unwrap();
        decoder.push(&chunk).unwrap();
    }
}

async fn fixture() -> (CursorProvider, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut connection = h2::server::handshake(socket).await.unwrap();
        while let Some(accepted) = connection.accept().await {
            let (request, mut respond) = accepted.unwrap();
            tokio::spawn(async move {
                assert_eq!(request.uri().path(), "/agent.v1.AgentService/Run");
                assert_eq!(request.headers()["authorization"], "Bearer test-token");
                let mut recv = request.into_body();
                let mut decoder = connect::Decoder::default();
                let initial = receive_message(&mut recv, &mut decoder).await;
                let run = initial.run.unwrap();
                assert_eq!(run.tools.unwrap().tools[0].name, "host_tool");
                let state = wire::State::decode(run.state.as_slice()).unwrap();
                assert_eq!(state.roots.len(), 1);
                let response = http::Response::builder()
                    .status(200)
                    .header("content-type", "application/connect+proto")
                    .body(())
                    .unwrap();
                let mut send = respond.send_response(response, false).unwrap();
                // Catalog refresh is protocol setup, and may only describe the
                // tools the host actually registered for this turn.
                send_message(
                    &mut send,
                    wire::ServerMessage {
                        exec: Some(
                            wire::ExecServer {
                                id: 4,
                                mcp_state: Some(wire::McpStateArgs::default()),
                                ..Default::default()
                            }
                            .encode_to_vec(),
                        ),
                        ..Default::default()
                    },
                    false,
                )
                .await;
                let state_reply = receive_message(&mut recv, &mut decoder)
                    .await
                    .exec
                    .unwrap()
                    .mcp_state
                    .unwrap()
                    .success
                    .unwrap();
                assert_eq!(state_reply.servers.len(), 1);
                assert_eq!(state_reply.servers[0].tools.len(), 1);
                assert_eq!(state_reply.servers[0].tools[0].name, "host_tool");
                // A real request for the system blob, then a server-owned blob.
                send_message(
                    &mut send,
                    wire::ServerMessage {
                        kv: Some(wire::KvServer {
                            id: 1,
                            get: Some(wire::GetBlob {
                                id: state.roots[0].clone(),
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    false,
                )
                .await;
                let blob = receive_message(&mut recv, &mut decoder)
                    .await
                    .kv
                    .unwrap()
                    .get
                    .unwrap()
                    .data
                    .unwrap();
                assert!(std::str::from_utf8(&blob).unwrap().contains("Be helpful"));
                let data = b"persisted server blob".to_vec();
                use sha2::Digest;
                send_message(
                    &mut send,
                    wire::ServerMessage {
                        kv: Some(wire::KvServer {
                            id: 2,
                            set: Some(wire::SetBlob {
                                id: sha2::Sha256::digest(&data).to_vec(),
                                data,
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    false,
                )
                .await;
                assert!(
                    receive_message(&mut recv, &mut decoder)
                        .await
                        .kv
                        .unwrap()
                        .set
                        .is_some()
                );
                let exec = wire::ExecServer {
                    id: 3,
                    exec_id: "exec-3".into(),
                    mcp: Some(wire::McpArgs {
                        name: "host_tool".into(),
                        tool_name: "host_tool".into(),
                        provider: MCP_PROVIDER.into(),
                        call_id: "call-3".into(),
                        args: [("count".into(), json_value(&json!(7)).encode_to_vec())]
                            .into_iter()
                            .collect(),
                    }),
                    ..Default::default()
                };
                send_message(
                    &mut send,
                    wire::ServerMessage {
                        exec: Some(exec.encode_to_vec()),
                        ..Default::default()
                    },
                    false,
                )
                .await;
                let result = receive_message(&mut recv, &mut decoder).await.exec.unwrap();
                assert_eq!(result.id, 3);
                assert_eq!(result.exec_id, "exec-3");
                assert_eq!(
                    result.mcp.unwrap().success.unwrap().content[0]
                        .text
                        .as_ref()
                        .unwrap()
                        .text,
                    "{\"answer\":7}"
                );
                // Redelivery must reply from the completed journal without a
                // second Event::ToolCall or host side effect.
                send_message(
                    &mut send,
                    wire::ServerMessage {
                        exec: Some(exec.encode_to_vec()),
                        ..Default::default()
                    },
                    false,
                )
                .await;
                assert!(
                    receive_message(&mut recv, &mut decoder)
                        .await
                        .exec
                        .unwrap()
                        .mcp
                        .is_some()
                );
                send_message(
                    &mut send,
                    wire::ServerMessage {
                        interaction: Some(wire::Interaction {
                            text: Some(wire::Text {
                                text: "done".into(),
                            }),
                            ended: Some(wire::TurnEnded {
                                input: Some(10),
                                output: Some(2),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    false,
                )
                .await;
                send_message(
                    &mut send,
                    wire::ServerMessage {
                        checkpoint: Some(run.state),
                        ..Default::default()
                    },
                    false,
                )
                .await;
                send.send_data(Bytes::from_static(&[2, 0, 0, 0, 2, b'{', b'}']), true)
                    .unwrap();
            });
        }
    });
    (
        CursorProvider::new(Options {
            base_url,
            ..Default::default()
        })
        .unwrap(),
        task,
    )
}

#[tokio::test]
async fn bidirectional_h2_tools_blob_ack_checkpoint_and_duplicate_redelivery() {
    let (provider, peer) = fixture().await;
    let mut session = provider.start(request(), context()).await.unwrap();
    let mut checkpoints = Vec::new();
    let mut calls = 0;
    let mut text = String::new();
    let mut completed = false;
    for _ in 0..30 {
        let event = tokio::time::timeout(Duration::from_secs(5), session.next_event())
            .await
            .unwrap()
            .unwrap();
        match event {
            Some(Event::Checkpoint(checkpoint)) => checkpoints.push(checkpoint),
            Some(Event::ToolCall(call)) => {
                calls += 1;
                assert_eq!(call.arguments, json!({"count":7.0}));
                assert!(
                    checkpoints.last().unwrap().state["executions"]["call-3"]["result"].is_null()
                );
                session
                    .submit_tool_result(ToolResult {
                        call_id: call.id,
                        output: json!({"answer":7}),
                        is_error: false,
                    })
                    .await
                    .unwrap();
            }
            Some(Event::TextDelta(delta)) => text.push_str(&delta),
            Some(Event::Usage(usage)) => assert_eq!(usage.input_tokens, 10),
            Some(Event::Completed { .. }) => {
                completed = true;
                break;
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }
    assert!(completed);
    assert_eq!(calls, 1);
    assert_eq!(text, "done");
    assert!(
        !checkpoints.last().unwrap().state["checkpoint"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    let mut resumed = request();
    resumed.checkpoint = checkpoints.pop();
    let restored = state::State::restore(&resumed).unwrap();
    assert!(restored.executions["call-3"].result.is_some());
    assert_eq!(restored.blobs.len(), 2);
    assert!(session.next_event().await.unwrap().is_none());
    peer.abort();
}

fn snapshot(request: &Request, pending: bool) -> Checkpoint {
    let mut state = state::State::restore(request).unwrap();
    state.put_blob(b"data").unwrap();
    state.executions.insert(
        "call".into(),
        state::Execution {
            call: ToolCall {
                id: "call".into(),
                name: "host_tool".into(),
                arguments: json!({}),
            },
            result: (!pending).then(|| ToolResult {
                call_id: "call".into(),
                output: json!("done"),
                is_error: false,
            }),
        },
    );
    state
        .snapshot(&request.namespace, &state::hash(&request.messages).unwrap())
        .unwrap()
}

#[test]
fn checkpoint_rejects_identity_prefix_corruption_and_unknown_execution_outcome() {
    let mut req = request();
    req.checkpoint = Some(snapshot(&req, true));
    assert!(
        matches!(state::State::restore(&req), Err(ProviderError::Checkpoint(message)) if message.contains("unknown outcome"))
    );
    req.checkpoint = Some(snapshot(&request(), false));
    req.namespace.account = "different".into();
    assert!(state::State::restore(&req).is_err());
    req = request();
    req.checkpoint = Some(snapshot(&req, false));
    req.messages[0].content[0] = Content::Text {
        text: "changed".into(),
    };
    assert!(state::State::restore(&req).is_err());
    req = request();
    req.checkpoint = Some(snapshot(&req, false));
    let checkpoint = req.checkpoint.as_mut().unwrap();
    let key = checkpoint.state["blobs"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    checkpoint.state["blobs"][key] = Value::String(STANDARD.encode(b"corrupt"));
    assert!(state::State::restore(&req).is_err());
}

#[test]
fn cursor_arg_wire_formats_keep_numbers_booleans_arrays_and_json_strings() {
    let args = [
        ("number".into(), json_value(&json!(3)).encode_to_vec()),
        ("boolean".into(), json_value(&json!(false)).encode_to_vec()),
        (
            "array".into(),
            json_value(&json!([1, "two"])).encode_to_vec(),
        ),
        ("json".into(), br#"{"key":true}"#.to_vec()),
        ("string".into(), b"hello".to_vec()),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        decode_args(args).unwrap(),
        json!({"number":3.0,"boolean":false,"array":[1.0,"two"],"json":{"key":true},"string":"hello"})
    );
}

#[test]
fn native_requests_only_map_to_explicit_host_aliases() {
    let exec = wire::ExecServer {
        id: 1,
        read: Some(wire::ReadArgs {
            path: "file".into(),
            call_id: "read-1".into(),
            offset: Some(2),
            limit: Some(4),
        }),
        ..Default::default()
    };
    let (call, operation) = native::call(&exec).unwrap().unwrap();
    assert_eq!(call.name, native::READ);
    assert_eq!(call.arguments, json!({"path":"file","offset":2,"limit":4}));
    let responses = native::response(
        1,
        "read-exec",
        &operation,
        &ToolResult {
            call_id: "read-1".into(),
            output: json!({"content":"text","total_lines":1,"file_size":4}),
            is_error: false,
        },
    )
    .unwrap();
    assert!(responses[0].exec.as_ref().unwrap().read.is_some());
}

#[test]
fn input_images_use_selected_context_and_content_addressed_blobs() {
    let mut req = request();
    req.messages[0].content.push(Content::Image {
        url: format!("data:image/png;base64,{}", STANDARD.encode(b"png-fixture")),
    });
    let mut state = state::State::restore(&req).unwrap();
    let (run, _) = prepare(&req, &mut state).unwrap();
    let selected = run
        .action
        .unwrap()
        .user
        .unwrap()
        .message
        .unwrap()
        .selected_context
        .unwrap();
    assert_eq!(selected.images.len(), 1);
    assert_eq!(selected.images[0].mime, "image/png");
    let blob = selected.images[0].blob.as_ref().unwrap();
    assert_eq!(state.blob(&blob.id).unwrap().unwrap(), b"png-fixture");
}

#[tokio::test]
async fn cancelled_start_never_opens_an_upstream_connection() {
    let ctx = context();
    ctx.cancellation.cancel();
    let provider = CursorProvider::new(Options {
        base_url: "http://127.0.0.1:1".into(),
        ..Default::default()
    })
    .unwrap();
    assert!(matches!(
        provider.start(request(), ctx).await,
        Err(ProviderError::Cancelled)
    ));
}

#[test]
fn absent_native_options_are_omitted_and_shell_hard_timeout_is_preserved() {
    let read = wire::ExecServer {
        read: Some(wire::ReadArgs {
            path: "file".into(),
            call_id: "read".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        native::call(&read).unwrap().unwrap().0.arguments,
        json!({"path":"file"})
    );
    let mut shell = wire::ExecServer {
        shell_stream: Some(wire::ShellArgs {
            command: "true".into(),
            call_id: "shell".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        native::call(&shell).unwrap().unwrap().0.arguments,
        json!({"command":"true"})
    );
    shell.shell_stream.as_mut().unwrap().hard_timeout = Some(500);
    assert_eq!(
        native::call(&shell).unwrap().unwrap().0.arguments,
        json!({"command":"true","timeout_ms":500})
    );
    shell.shell_stream.as_mut().unwrap().background = true;
    assert!(matches!(
        native::call(&shell),
        Err(ProviderError::Unsupported(_))
    ));
}

#[test]
fn native_shell_stream_finishes_with_structured_result_and_close_even_when_denied() {
    let operation = native::Operation::Shell {
        command: "true".into(),
        cwd: "/work".into(),
        stream: true,
    };
    for (is_error, output) in [
        (false, json!({"exit_code":0,"stdout":"ok","stderr":""})),
        (true, json!({"rejected":true,"reason":"denied"})),
    ] {
        let messages = native::response(
            5,
            "exec",
            &operation,
            &ToolResult {
                call_id: "shell".into(),
                is_error,
                output,
            },
        )
        .unwrap();
        assert_eq!(messages.len(), 6);
        #[derive(prost::Message)]
        struct Start {
            #[prost(bytes = "vec", optional, tag = "4")]
            value: Option<Vec<u8>>,
        }
        let first = Start::decode(
            messages[0]
                .exec
                .as_ref()
                .unwrap()
                .shell_stream
                .as_ref()
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        assert_eq!(first.value, Some(vec![]));
        assert!(messages[4].exec.as_ref().unwrap().shell.is_some());
        assert_eq!(
            messages[5]
                .exec_control
                .as_ref()
                .unwrap()
                .stream_close
                .as_ref()
                .unwrap()
                .id,
            5
        );
    }
    assert!(
        native::response(
            5,
            "exec",
            &operation,
            &ToolResult {
                call_id: "shell".into(),
                is_error: false,
                output: json!({})
            }
        )
        .is_err()
    );
}

#[tokio::test]
async fn native_shell_stream_continues_on_same_h2_session_after_final_ack() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut connection = h2::server::handshake(socket).await.unwrap();
        let (request, mut respond) = connection.accept().await.unwrap().unwrap();
        let _handler = tokio::spawn(async move {
            let mut recv = request.into_body();
            let mut decoder = connect::Decoder::default();
            assert!(receive_message(&mut recv, &mut decoder).await.run.is_some());
            let response = http::Response::builder()
                .status(200)
                .header("content-type", "application/connect+proto")
                .body(())
                .unwrap();
            let mut send = respond.send_response(response, false).unwrap();
            send_message(
                &mut send,
                wire::ServerMessage {
                    exec: Some(
                        wire::ExecServer {
                            id: 8,
                            exec_id: "native-shell-exec".into(),
                            shell_stream: Some(wire::ShellArgs {
                                command: "printf ok".into(),
                                call_id: "native-shell".into(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }
                        .encode_to_vec(),
                    ),
                    ..Default::default()
                },
                false,
            )
            .await;
            for _ in 0..4 {
                assert!(
                    receive_message(&mut recv, &mut decoder)
                        .await
                        .exec
                        .unwrap()
                        .shell_stream
                        .is_some()
                );
            }
            assert!(
                receive_message(&mut recv, &mut decoder)
                    .await
                    .exec
                    .unwrap()
                    .shell
                    .is_some()
            );
            assert_eq!(
                receive_message(&mut recv, &mut decoder)
                    .await
                    .exec_control
                    .unwrap()
                    .stream_close
                    .unwrap()
                    .id,
                8
            );
            send_message(
                &mut send,
                wire::ServerMessage {
                    interaction: Some(wire::Interaction {
                        text: Some(wire::Text {
                            text: "continued".into(),
                        }),
                        ended: Some(wire::TurnEnded::default()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                false,
            )
            .await;
            send.send_data(Bytes::from_static(&[2, 0, 0, 0, 2, b'{', b'}']), true)
                .unwrap();
        });
        while let Some(result) = connection.accept().await {
            result.unwrap();
        }
    });
    let provider = CursorProvider::new(Options {
        base_url,
        ..Default::default()
    })
    .unwrap();
    let mut req = request();
    req.tools = vec![ToolDefinition {
        name: native::SHELL.into(),
        description: "Shell".into(),
        parameters: json!({"type":"object"}),
    }];
    let mut session = provider.start(req, context()).await.unwrap();
    let mut host_calls = 0;
    let mut text = String::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), session.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Event::ToolCall(call) => {
                host_calls += 1;
                assert_eq!(call.name, native::SHELL);
                assert_eq!(call.arguments, json!({"command":"printf ok"}));
                session
                    .submit_tool_result(ToolResult {
                        call_id: call.id,
                        is_error: false,
                        output: json!({"exit_code":0,"stdout":"ok","stderr":""}),
                    })
                    .await
                    .unwrap();
            }
            Event::TextDelta(delta) => text.push_str(&delta),
            Event::Completed { .. } => break,
            _ => {}
        }
    }
    assert_eq!(host_calls, 1);
    assert_eq!(text, "continued");
    server.abort();
}

#[test]
fn nested_protobuf_strings_keep_their_original_types() {
    let value = json!({"number_text":"123","boolean_text":"true","json_text":"{\"a\":1}","values":["false","null","42"]});
    assert_eq!(from_value(json_value(&value)).unwrap(), value);
    assert_eq!(from_value(json_value(&json!("123"))).unwrap(), json!(123));
}
