use codex_provider_api as api;
use serde_json::{Value, json};
use std::sync::Arc;

struct FixtureOperations;
impl api::NativeOperations for FixtureOperations {
    fn definitions(&self) -> Vec<api::ToolDefinition> {
        vec![]
    }
    fn operation(&self, call: &api::ToolCall) -> api::Result<api::HostOperation> {
        Ok(api::HostOperation::Shell(call.arguments.clone()))
    }
    fn result(&self, _: &api::ToolCall, structured: &Value) -> api::Result<Value> {
        Ok(structured.clone())
    }
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
    super::validate_lineage(&mut request, api::RecoveryPolicy::RestorePrevious).unwrap();
    assert!(
        matches!(&request.messages[0].content[0], api::Content::ToolResult(result) if result.is_error)
    );
    assert_eq!(request.metadata["host_checkpoint_prefix"], 1);
    let api::Content::ToolResult(result) = &mut request.messages[0].content[0] else {
        panic!()
    };
    result.output = json!("edited after checkpoint");
    assert!(super::validate_lineage(&mut request, api::RecoveryPolicy::RestorePrevious).is_err());
}

#[test]
fn interrupted_text_generation_recovers_only_a_verified_clean_checkpoint() {
    let mut request = bound_request();
    let clean = request.checkpoint.clone().unwrap();
    request.checkpoint.as_mut().unwrap().state = json!({"codex_host_pending":true,"codex_host_tools_started":false,"codex_host_previous":clean});
    super::validate_lineage(&mut request, api::RecoveryPolicy::RestorePrevious).unwrap();
    assert_eq!(request.checkpoint, Some(clean));
    request.checkpoint.as_mut().unwrap().state = json!({"codex_host_pending":true,"codex_host_tools_started":false,"codex_host_previous":null});
    super::validate_lineage(&mut request, api::RecoveryPolicy::RestorePrevious).unwrap();
    assert!(request.checkpoint.is_none());
}

#[test]
fn interrupted_tools_and_unverified_checkpoints_refuse_replay() {
    for started in [json!(true), Value::Null] {
        let mut request = bound_request();
        request.checkpoint.as_mut().unwrap().state =
            json!({"codex_host_pending":true,"codex_host_tools_started":started});
        assert!(
            super::validate_lineage(&mut request, api::RecoveryPolicy::RestorePrevious).is_err()
        );
    }
    let mut request = bound_request();
    request.checkpoint.as_mut().unwrap().state = json!({});
    assert!(super::validate_lineage(&mut request, api::RecoveryPolicy::RestorePrevious).is_err());
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
        operations: None,
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
        native_tools: ["native_shell".into()].into_iter().collect(),
        operations: Some(Arc::new(FixtureOperations)),
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
                name: "native_shell".into(),
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
            .send(api::HostCompletion {
                call_id: call.call.id,
                output: json!("recorded history"),
                structured: Some(json!({"output":output,"session_id":session_id,"exit_code":0})),
                disposition: api::CompletionDisposition::Success,
            })
            .unwrap();
    }
    let result = executing.await.unwrap().unwrap();
    assert!(result.is_error);
    assert_eq!(result.output, "native operation output exceeded 2 MiB");
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
    super::validate_lineage(&mut request, api::RecoveryPolicy::RestorePrevious).unwrap();
    assert_eq!(request.messages, history);
    assert_eq!(request.metadata["host_checkpoint_prefix"], 1);
    let mut edited = request.clone();
    if let api::Content::ToolResult(result) = &mut edited.messages[0].content[0] {
        result.output = json!("allowed");
    }
    assert!(super::validate_lineage(&mut edited, api::RecoveryPolicy::RestorePrevious).is_err());
    let mut interrupted = request.clone();
    interrupted.checkpoint.as_mut().unwrap().state["codex_host_pending"] = json!(true);
    assert!(
        super::validate_lineage(&mut interrupted, api::RecoveryPolicy::RestorePrevious).is_err()
    );
    request.messages.clear();
    assert!(super::validate_lineage(&mut request, api::RecoveryPolicy::RestorePrevious).is_err());
}

#[test]
fn interrupted_text_rebuild_policy_does_not_depend_on_provider_id() {
    let mut request = bound_request();
    let clean = request.checkpoint.clone().unwrap();
    request.checkpoint.as_mut().unwrap().state = json!({"codex_host_pending":true,"codex_host_tools_started":false,"codex_host_previous":clean});
    super::validate_lineage(&mut request, api::RecoveryPolicy::RebuildFromHistory).unwrap();
    assert!(request.checkpoint.is_none());
}
