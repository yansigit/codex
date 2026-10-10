use super::{call_item, messages, tool_result, tools};
use crate::client_common::Prompt;
use codex_protocol::models::{FunctionCallOutputBody, FunctionCallOutputPayload, ResponseItem};
use codex_provider_api as api;
use codex_tools::{
    FreeformTool, FreeformToolFormat, JsonSchema, ResponsesApiNamespace, ResponsesApiNamespaceTool,
    ResponsesApiTool, ToolSpec,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn item(value: Value) -> ResponseItem {
    serde_json::from_value(value).expect("valid response item")
}

fn bound_request() -> api::Request {
    let history = vec![api::Message {
        role: api::Role::Tool,
        content: vec![api::Content::ToolResult(api::ToolResult {
            call_id: "failed-call".into(),
            output: json!("denied"),
            is_error: true,
        })],
    }];
    let namespace = api::Namespace {
        provider: "claude".into(),
        account: "fixture".into(),
        model: "model".into(),
        thread: "thread".into(),
    };
    let checkpoint = api::Checkpoint {
        version: 1,
        namespace: namespace.clone(),
        context_hash: "provider-prefix".into(),
        state: json!({
            "codex_host_pending":false,
            "codex_host_lineage": { "length":history.len(), "hash":super::lineage_hash("rules", &history).unwrap(), "failed_results":super::failed_result_bindings(&history).unwrap() }
        }),
    };
    api::Request {
        namespace,
        system: "rules".into(),
        messages: history,
        tools: vec![],
        parallel_tool_calls: false,
        max_output_tokens: None,
        reasoning_effort: None,
        output_schema: None,
        checkpoint: Some(checkpoint),
        metadata: json!({}),
    }
}

#[test]
fn checkpoint_lineage_restores_failure_flags_but_rejects_changed_output() {
    let mut request = bound_request();
    let api::Content::ToolResult(result) = &mut request.messages[0].content[0] else {
        panic!()
    };
    result.is_error = false; // Ordinary rollout serialization drops the failure flag.
    super::validate_lineage(&mut request).unwrap();
    assert!(
        matches!(&request.messages[0].content[0], api::Content::ToolResult(result) if result.is_error)
    );
    assert_eq!(request.metadata["host_checkpoint_prefix"], 1);
    let api::Content::ToolResult(result) = &mut request.messages[0].content[0] else {
        panic!()
    };
    result.output = json!("edited after checkpoint");
    assert!(super::validate_lineage(&mut request).is_err());
}

#[test]
fn interrupted_text_generation_recovers_only_a_verified_clean_checkpoint() {
    let mut request = bound_request();
    let clean = request.checkpoint.clone().unwrap();
    request.checkpoint.as_mut().unwrap().state = json!({"codex_host_pending":true,"codex_host_tools_started":false,"codex_host_previous":clean});
    super::validate_lineage(&mut request).unwrap();
    assert_eq!(request.checkpoint, Some(clean));
    request.checkpoint.as_mut().unwrap().state = json!({"codex_host_pending":true,"codex_host_tools_started":false,"codex_host_previous":null});
    super::validate_lineage(&mut request).unwrap();
    assert!(request.checkpoint.is_none());
}

#[test]
fn interrupted_tools_and_unverified_checkpoints_refuse_replay() {
    for started in [json!(true), Value::Null] {
        let mut request = bound_request();
        request.checkpoint.as_mut().unwrap().state =
            json!({"codex_host_pending":true,"codex_host_tools_started":started});
        assert!(super::validate_lineage(&mut request).is_err());
    }
    let mut request = bound_request();
    request.checkpoint.as_mut().unwrap().state = json!({});
    assert!(super::validate_lineage(&mut request).is_err());
}

#[tokio::test]
async fn checkpoint_after_tool_dispatch_cannot_clear_interruption_fence() {
    use api::Host;
    let home = tempfile::tempdir().unwrap();
    let request = bound_request();
    let store = codex_provider_auth::SessionStore::new(home.path(), request.namespace.clone());
    let state = Arc::new(tokio::sync::Mutex::new(super::TurnCheckpoint {
        namespace: request.namespace.clone(),
        latest: None,
        previous: request.checkpoint.clone(),
        tools_started: false,
    }));
    let (calls, mut receive) = tokio::sync::mpsc::channel(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    let host = Arc::new(super::ChannelHost {
        store: store.clone(),
        calls,
        cancellation: cancel.clone(),
        poll_tool: None,
        native_tools: Default::default(),
        checkpoint: state,
        execution: Arc::new(store.begin_execution("turn", &cancel).await.unwrap()),
    });
    let execute_host = host.clone();
    let execute_cancel = cancel.clone();
    let executing = tokio::spawn(async move {
        execute_host
            .execute(
                api::ToolCall {
                    id: "one".into(),
                    name: "exec".into(),
                    arguments: json!({}),
                },
                execute_cancel,
            )
            .await
    });
    let call = receive.recv().await.unwrap();
    assert_eq!(
        store.load().unwrap().unwrap().state["codex_host_tools_started"],
        true
    );
    host.checkpoint(request.checkpoint.unwrap()).await.unwrap();
    assert_eq!(
        store.load().unwrap().unwrap().state["codex_host_tools_started"],
        true
    );
    cancel.cancel();
    drop(call);
    assert!(executing.await.unwrap().is_err());
}

#[tokio::test]
async fn oversized_native_output_drains_the_foreground_process_before_returning_error() {
    use api::Host;
    let home = tempfile::tempdir().unwrap();
    let request = bound_request();
    let store = codex_provider_auth::SessionStore::new(home.path(), request.namespace.clone());
    let cancel = tokio_util::sync::CancellationToken::new();
    let (calls, mut receive) = tokio::sync::mpsc::channel(1);
    let host = super::ChannelHost {
        store: store.clone(),
        calls,
        cancellation: cancel.clone(),
        poll_tool: Some("write_stdin".into()),
        native_tools: ["cursor_shell".into()].into_iter().collect(),
        checkpoint: Arc::new(tokio::sync::Mutex::new(super::TurnCheckpoint {
            namespace: request.namespace,
            latest: None,
            previous: None,
            tools_started: false,
        })),
        execution: Arc::new(store.begin_execution("turn", &cancel).await.unwrap()),
    };
    let executing = tokio::spawn(async move {
        host.execute(
            api::ToolCall {
                id: "large-output".into(),
                name: "cursor_shell".into(),
                arguments: json!({"command":"fixture"}),
            },
            cancel,
        )
        .await
    });
    for (index, (output, session_id)) in [
        ("x".repeat(2 * 1024 * 1024 + 1), Some(7)),
        ("still running".into(), Some(7)),
        ("finished".into(), None),
    ]
    .into_iter()
    .enumerate()
    {
        let call = tokio::time::timeout(std::time::Duration::from_secs(2), receive.recv())
            .await
            .expect("host must keep draining")
            .expect("foreground operation must not return before exit");
        if index > 0 {
            assert_eq!(call.call.name, "write_stdin");
            assert_eq!(call.call.arguments["session_id"], 7);
        }
        call.reply
            .send(api::ToolResult {
                call_id: call.call.id,
                output: json!({"codex_native_structured":{
                    "output":output,"session_id":session_id,"exit_code":0
                }}),
                is_error: false,
            })
            .unwrap();
    }
    let result = executing.await.unwrap().unwrap();
    assert!(result.is_error);
    assert_eq!(result.output, "native operation output exceeded 2 MiB");
}

#[test]
fn messages_map_roles_images_and_adjacent_assistant_items() {
    let input = vec![
        item(
            json!({"type":"message","role":"system","content":[{"type":"input_text","text":"rules"}]}),
        ),
        item(json!({"type":"message","role":"user","content":[
            {"type":"input_text","text":"look"},
            {"type":"input_image","image_url":"data:image/png;base64,AA=="}
        ]})),
        item(
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"checking"}]}),
        ),
        item(
            json!({"type":"reasoning","summary":[{"type":"summary_text","text":"thought"}],"encrypted_content":null}),
        ),
        item(
            json!({"type":"function_call","call_id":"c1","name":"read","arguments":"{\"path\":\"a\"}"}),
        ),
    ];

    let mapped = messages(&input).expect("messages convert");
    assert_eq!(mapped.len(), 3);
    assert_eq!(mapped[0].role, api::Role::System);
    assert_eq!(
        mapped[0].content,
        vec![api::Content::Text {
            text: "rules".into()
        }]
    );
    assert_eq!(mapped[1].role, api::Role::User);
    assert_eq!(
        mapped[1].content,
        vec![
            api::Content::Text {
                text: "look".into()
            },
            api::Content::Image {
                url: "data:image/png;base64,AA==".into()
            },
        ]
    );
    assert_eq!(mapped[2].role, api::Role::Assistant);
    assert_eq!(
        mapped[2].content,
        vec![
            api::Content::Text {
                text: "checking".into()
            },
            api::Content::Thinking {
                text: "thought".into(),
                signature: None
            },
            api::Content::ToolCall(api::ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: json!({"path":"a"}),
            }),
        ]
    );
}

#[test]
fn tools_map_namespaced_functions_and_custom_tools() {
    let function = ResponsesApiTool {
        name: "lookup".into(),
        description: "find a thing".into(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::default(),
        output_schema: None,
    };
    let custom = FreeformTool {
        name: "script".into(),
        description: "run script".into(),
        defer_loading: None,
        format: FreeformToolFormat {
            r#type: "text".into(),
            syntax: "plain".into(),
            definition: "...".into(),
        },
    };
    let mut prompt = Prompt::default();
    prompt.tools = Arc::from([ToolSpec::Namespace(ResponsesApiNamespace {
        name: "local".into(),
        description: String::new(),
        tools: vec![
            ResponsesApiNamespaceTool::Function(function),
            ResponsesApiNamespaceTool::Custom(custom),
        ],
    })]);

    let (definitions, targets) = tools(&prompt).expect("tools convert");
    assert_eq!(
        definitions
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["local__lookup", "local__script"]
    );
    assert_eq!(targets["local__lookup"].namespace.as_deref(), Some("local"));
    assert!(!targets["local__lookup"].custom);
    assert!(targets["local__script"].custom);
    assert_eq!(
        definitions[1].parameters,
        json!({
            "type":"object", "properties":{"input":{"type":"string"}},
            "required":["input"], "additionalProperties":false
        })
    );

    let custom_call = call_item(
        api::ToolCall {
            id: "c2".into(),
            name: "local__script".into(),
            arguments: json!({"input":"print(1)"}),
        },
        &targets,
    )
    .expect("custom call maps back to the Responses item");
    assert_eq!(
        serde_json::to_value(custom_call).expect("serialize mapped call"),
        json!({
            "type":"custom_tool_call", "call_id":"c2", "name":"script",
            "namespace":"local", "input":"print(1)"
        })
    );
}

#[test]
fn failed_tool_outputs_remain_marked_as_errors() {
    let failed = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some("c1".into()),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("denied".into()),
            success: Some(false),
        },
        internal_chat_message_metadata_passthrough: None,
    };
    let converted = tool_result(&failed).expect("tool output converts");
    assert_eq!(converted.call_id, "c1");
    assert!(converted.is_error);
    assert_eq!(converted.output, json!("denied"));

    let mapped = messages(&[failed]).expect("history output converts");
    assert_eq!(mapped[0].role, api::Role::Tool);
    assert!(matches!(&mapped[0].content[0], api::Content::ToolResult(result) if result.is_error));
}

#[test]
fn unsupported_history_and_content_formats_are_rejected() {
    let audio = item(
        json!({"type":"message","role":"user","content":[{"type":"input_audio","audio_url":"data:audio/wav;base64,AA=="}]}),
    );
    assert!(messages(&[audio]).is_err());

    let agent_message = item(
        json!({"type":"agent_message","author":"a","recipient":"b","content":[{"type":"input_text","text":"opaque"}]}),
    );
    assert!(messages(&[agent_message]).is_err());

    let malformed_call =
        item(json!({"type":"function_call","call_id":"c1","name":"read","arguments":"not json"}));
    assert!(messages(&[malformed_call]).is_err());
}

#[test]
fn checkpoint_binding_restores_failed_results_and_rejects_edited_history() {
    let history = vec![api::Message {
        role: api::Role::Tool,
        content: vec![api::Content::ToolResult(api::ToolResult {
            call_id: "one".into(),
            output: json!("permission denied"),
            is_error: true,
        })],
    }];
    let namespace = api::Namespace {
        provider: "claude".into(),
        account: "a".into(),
        model: "m".into(),
        thread: "t".into(),
    };
    let mut request = api::Request {
        namespace: namespace.clone(),
        system: "system".into(),
        messages: history.clone(),
        tools: vec![],
        parallel_tool_calls: true,
        max_output_tokens: None,
        reasoning_effort: None,
        output_schema: None,
        metadata: json!({}),
        checkpoint: Some(api::Checkpoint {
            version: 1,
            namespace,
            context_hash: "provider-owned".into(),
            state: json!({"codex_host_pending":false,"codex_host_lineage":{"length":1,"hash":super::lineage_hash("system", &history).unwrap(),"failed_results":super::failed_result_bindings(&history).unwrap()}}),
        }),
    };
    if let api::Content::ToolResult(result) = &mut request.messages[0].content[0] {
        result.is_error = false;
    }
    super::validate_lineage(&mut request).unwrap();
    assert_eq!(request.messages, history);
    assert_eq!(request.metadata["host_checkpoint_prefix"], 1);
    let mut edited = request.clone();
    if let api::Content::ToolResult(result) = &mut edited.messages[0].content[0] {
        result.output = json!("allowed");
    }
    assert!(super::validate_lineage(&mut edited).is_err());
    let mut interrupted = request.clone();
    interrupted.checkpoint.as_mut().unwrap().state["codex_host_pending"] = json!(true);
    assert!(super::validate_lineage(&mut interrupted).is_err());
    request.messages.clear();
    assert!(super::validate_lineage(&mut request).is_err());
}

#[derive(Debug)]
struct MockNativeProvider {
    home: std::path::PathBuf,
    info: codex_model_provider_info::ModelProviderInfo,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    results: Arc<std::sync::Mutex<Vec<api::ToolResult>>>,
}

impl codex_model_provider::ModelProvider for MockNativeProvider {
    fn info(&self) -> &codex_model_provider_info::ModelProviderInfo {
        &self.info
    }
    fn auth_manager(&self) -> Option<Arc<codex_login::AuthManager>> {
        None
    }
    fn auth(
        &self,
    ) -> codex_model_provider::ModelProviderFuture<'_, Option<codex_login::CodexAuth>> {
        Box::pin(async { None })
    }
    fn account_state(&self) -> codex_model_provider::ProviderAccountResult {
        Ok(codex_model_provider::ProviderAccountState {
            account: None,
            requires_openai_auth: false,
        })
    }
    fn models_manager(
        &self,
        _: std::path::PathBuf,
        catalog: Option<codex_protocol::openai_models::ModelsResponse>,
    ) -> codex_models_manager::manager::SharedModelsManager {
        self.models_manager_without_cache(catalog)
    }
    fn native_state_home(&self) -> Option<std::path::PathBuf> {
        Some(self.home.clone())
    }
    fn native_credentials(
        &self,
        _: tokio_util::sync::CancellationToken,
    ) -> api::ProviderFuture<'_, api::Credentials> {
        Box::pin(async {
            Ok(api::Credentials {
                access_token: "fixture-token".into(),
                account_id: "fixture-account".into(),
                metadata: Value::Null,
            })
        })
    }
    fn native_provider(&self) -> api::Result<Arc<dyn api::Provider>> {
        Ok(Arc::new(MockBackend {
            cancelled: self.cancelled.clone(),
            results: self.results.clone(),
        }))
    }
}

struct MockBackend {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    results: Arc<std::sync::Mutex<Vec<api::ToolResult>>>,
}
impl api::Provider for MockBackend {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn models(&self, _: api::Context) -> api::ProviderFuture<'_, Vec<api::Model>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn start(
        &self,
        request: api::Request,
        _: api::Context,
    ) -> api::ProviderFuture<'_, Box<dyn api::ProviderSession>> {
        Box::pin(async move {
            assert_eq!(request.tools[0].name, "lookup");
            let session = MockSession {
                events: std::collections::VecDeque::from([
                    api::Event::TextDelta("before tool".into()),
                    api::Event::ToolCall(api::ToolCall {
                        id: "call-one".into(),
                        name: "lookup".into(),
                        arguments: json!({}),
                    }),
                ]),
                cancelled: self.cancelled.clone(),
                results: self.results.clone(),
            };
            Ok(Box::new(session) as Box<dyn api::ProviderSession>)
        })
    }
}
struct MockSession {
    events: std::collections::VecDeque<api::Event>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    results: Arc<std::sync::Mutex<Vec<api::ToolResult>>>,
}
impl api::ProviderSession for MockSession {
    fn next_event(&mut self) -> api::ProviderFuture<'_, Option<api::Event>> {
        Box::pin(async {
            match self.events.pop_front() {
                Some(event) => Ok(Some(event)),
                None => std::future::pending().await,
            }
        })
    }
    fn submit_tool_result(&mut self, result: api::ToolResult) -> api::ProviderFuture<'_, ()> {
        Box::pin(async move {
            self.results.lock().unwrap().push(result);
            self.events
                .push_back(api::Event::TextDelta("after tool".into()));
            self.events.push_back(api::Event::Completed {
                id: "finished".into(),
            });
            Ok(())
        })
    }
    fn cancel(&mut self) -> api::ProviderFuture<'_, ()> {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

fn mock_native(home: &std::path::Path) -> (Arc<MockNativeProvider>, Prompt) {
    let provider = Arc::new(MockNativeProvider {
        home: home.to_owned(),
        info: codex_model_provider_info::built_in_model_providers(None)
            .remove("claude")
            .unwrap(),
        cancelled: Arc::default(),
        results: Arc::default(),
    });
    let prompt = Prompt {
        tools: Arc::from([ToolSpec::Function(ResponsesApiTool {
            name: "lookup".into(),
            description: "lookup".into(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::default(),
            output_schema: None,
        })]),
        ..Prompt::default()
    };
    (provider, prompt)
}

#[tokio::test]
async fn native_stream_preserves_text_order_and_waits_for_host_result() {
    use crate::client_common::ResponseEvent;
    use futures::StreamExt;
    let home = tempfile::tempdir().unwrap();
    let (provider, prompt) = mock_native(home.path());
    let model = codex_models_manager::model_info::model_info_from_slug("fixture-model");
    let mut stream = super::stream(
        provider.clone(),
        &prompt,
        &model,
        "thread".into(),
        None,
        "window".into(),
        "turn",
    )
    .await
    .unwrap();
    let mut messages = vec![];
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match event {
            ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
                messages.extend(content)
            }
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { call_id, .. }) => {
                assert_eq!(call_id, "call-one");
                break;
            }
            ResponseEvent::Completed { .. } => panic!("completed before host result"),
            _ => {}
        }
    }
    assert_eq!(
        messages,
        vec![codex_protocol::models::ContentItem::OutputText {
            text: "before tool".into()
        }]
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), stream.next())
            .await
            .is_err()
    );
    // Core returns the ordinary history output together with structured executor
    // output. A standard tool must replay the history output, not this wrapper.
    stream.native_tool_results.as_ref().unwrap().send(api::ToolResult { call_id: "call-one".into(), output: json!({"codex_native_history":"denied","codex_native_structured":{"extra":"private runtime data"}}), is_error: true }).await.unwrap();
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let ResponseEvent::Completed {
            response_id,
            end_turn,
            ..
        } = event
        {
            assert_eq!(response_id, "finished");
            assert_eq!(end_turn, Some(true));
            break;
        }
    }
    let results = provider.results.lock().unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    assert_eq!(results[0].output, json!("denied"));
}

#[tokio::test]
async fn dropping_native_stream_cancels_pending_host_and_upstream() {
    use crate::client_common::ResponseEvent;
    use futures::StreamExt;
    let home = tempfile::tempdir().unwrap();
    let (provider, prompt) = mock_native(home.path());
    let model = codex_models_manager::model_info::model_info_from_slug("fixture-model");
    let mut stream = super::stream(
        provider.clone(),
        &prompt,
        &model,
        "thread".into(),
        None,
        "window".into(),
        "turn",
    )
    .await
    .unwrap();
    loop {
        if matches!(
            stream.next().await.unwrap().unwrap(),
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { .. })
        ) {
            break;
        }
    }
    drop(stream);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !provider.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(provider.results.lock().unwrap().is_empty());
    let changed_model = codex_models_manager::model_info::model_info_from_slug("other-model");
    let resumed = super::stream(
        provider,
        &prompt,
        &changed_model,
        "thread".into(),
        None,
        "new-compacted-window".into(),
        "turn",
    )
    .await;
    let Err(error) = resumed else {
        panic!("model/window changes must not bypass interrupted execution");
    };
    assert!(error.to_string().contains("explicitly branch"));
}
