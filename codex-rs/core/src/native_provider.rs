//! Native sessions use the ordinary streamed tool-call path in the turn runner.
//! The channel host waits for the persisted result while upstream remains open.
use std::collections::BTreeMap;
use std::sync::Arc;

use codex_model_provider::SharedModelProvider;
use codex_protocol::ResponseItemId;
use codex_protocol::error::{CodexErr, Result};
use codex_protocol::models::{ContentItem, ResponseItem};
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::TokenUsage;
use codex_provider_api as api;
use codex_provider_auth::{SessionStore, ToolDisposition};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::client_common::{Prompt, ResponseEvent, ResponseStream};

#[path = "native_provider_tools.rs"]
mod native_tools;

fn error(error: impl std::fmt::Display) -> CodexErr {
    CodexErr::Stream(error.to_string())
}

#[derive(Clone)]
struct ToolTarget {
    name: String,
    namespace: Option<String>,
    custom: bool,
    native: Option<String>,
}

fn tool_name(namespace: Option<&str>, name: &str) -> String {
    match namespace {
        Some(namespace) => format!("{namespace}__{name}"),
        None => name.to_owned(),
    }
}

fn native_operations_available() -> bool {
    cfg!(target_os = "linux")
        && std::path::Path::new("/bin/bash").is_file()
        && [
            "base64", "find", "head", "mktemp", "od", "rg", "rm", "rmdir", "sed", "tail",
            "timeout", "tr", "wc",
        ]
        .iter()
        .all(|name| which::which(name).is_ok())
}

fn tools(prompt: &Prompt) -> Result<(Vec<api::ToolDefinition>, BTreeMap<String, ToolTarget>)> {
    let mut definitions = vec![];
    let mut targets = BTreeMap::new();
    for spec in prompt.tools.iter() {
        let spec = serde_json::to_value(spec).map_err(error)?;
        let (namespace, entries) = if spec["type"] == "namespace" {
            (
                spec["name"].as_str(),
                spec["tools"].as_array().cloned().unwrap_or_default(),
            )
        } else {
            (None, vec![spec.clone()])
        };
        for entry in entries {
            let kind = entry["type"].as_str().unwrap_or("function");
            if kind != "function" && kind != "custom" {
                continue;
            }
            let Some(name) = entry["name"].as_str() else {
                return Err(error("native tool is missing a name"));
            };
            let mapped = tool_name(namespace, name);
            if targets
                .insert(
                    mapped.clone(),
                    ToolTarget {
                        name: name.into(),
                        namespace: namespace.map(str::to_owned),
                        custom: kind == "custom",
                        native: None,
                    },
                )
                .is_some()
            {
                return Err(error("native tool name collision"));
            }
            definitions.push(api::ToolDefinition {
                name: mapped,
                description: entry["description"].as_str().unwrap_or_default().into(),
                parameters: if kind == "custom" { json!({"type":"object","properties":{"input":{"type":"string"}},"required":["input"],"additionalProperties":false}) } else { entry["parameters"].clone() },
            });
        }
    }
    Ok((definitions, targets))
}

fn push(messages: &mut Vec<api::Message>, role: api::Role, content: Vec<api::Content>) {
    if content.is_empty() {
        return;
    }
    if let Some(previous) = messages.last_mut()
        && previous.role == role
    {
        previous.content.extend(content);
    } else {
        messages.push(api::Message { role, content });
    }
}

fn messages(input: &[ResponseItem]) -> Result<Vec<api::Message>> {
    let mut messages = vec![];
    for item in input {
        let failed = match item {
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => output.success == Some(false),
            _ => false,
        };
        let item = serde_json::to_value(item).map_err(error)?;
        match item["type"].as_str().unwrap_or_default() {
            "message" => {
                let role = match item["role"].as_str() {
                    Some("assistant") => api::Role::Assistant,
                    Some("system" | "developer") => api::Role::System,
                    Some("user") => api::Role::User,
                    _ => return Err(error("unsupported native message role")),
                };
                let mut content = vec![];
                for part in item["content"].as_array().into_iter().flatten() {
                    match part["type"].as_str() {
                        Some("input_text" | "output_text") => content.push(api::Content::Text {
                            text: part["text"].as_str().unwrap_or_default().into(),
                        }),
                        Some("input_image") => content.push(api::Content::Image {
                            url: part["image_url"].as_str().unwrap_or_default().into(),
                        }),
                        _ => {
                            return Err(error(
                                "native provider does not support this content type",
                            ));
                        }
                    }
                }
                push(&mut messages, role, content);
            }
            "function_call" | "custom_tool_call" => {
                let arguments = if item["type"] == "custom_tool_call" {
                    json!({"input":item["input"]})
                } else {
                    serde_json::from_str(item["arguments"].as_str().unwrap_or("{}"))
                        .map_err(error)?
                };
                push(
                    &mut messages,
                    api::Role::Assistant,
                    vec![api::Content::ToolCall(api::ToolCall {
                        id: item["call_id"].as_str().unwrap_or_default().into(),
                        name: tool_name(
                            item["namespace"].as_str(),
                            item["name"].as_str().unwrap_or_default(),
                        ),
                        arguments,
                    })],
                );
            }
            "function_call_output" | "custom_tool_call_output" => {
                push(
                    &mut messages,
                    api::Role::Tool,
                    vec![api::Content::ToolResult(api::ToolResult {
                        call_id: item["call_id"].as_str().unwrap_or_default().into(),
                        output: item["output"].clone(),
                        is_error: failed,
                    })],
                );
            }
            "reasoning" => {
                let text = item["summary"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<String>();
                if !text.is_empty() {
                    push(
                        &mut messages,
                        api::Role::Assistant,
                        vec![api::Content::Thinking {
                            text,
                            signature: None,
                        }],
                    );
                }
            }
            // Server-specific encrypted and bookkeeping items cannot cross provider boundaries.
            "compaction"
            | "context_compaction"
            | "configuration_update"
            | "compaction_trigger"
            | "additional_tools" => {}
            other => {
                return Err(error(format!(
                    "native history contains unsupported item: {other}"
                )));
            }
        }
    }
    Ok(messages)
}

struct HostCall {
    call: api::ToolCall,
    reply: oneshot::Sender<api::ToolResult>,
}
struct TurnCheckpoint {
    namespace: api::Namespace,
    latest: Option<api::Checkpoint>,
    previous: Option<api::Checkpoint>,
    tools_started: bool,
}
impl TurnCheckpoint {
    fn pending(&self, mut checkpoint: api::Checkpoint) -> api::Checkpoint {
        checkpoint.state["codex_host_pending"] = json!(true);
        checkpoint.state["codex_host_tools_started"] = json!(self.tools_started);
        checkpoint.state["codex_host_previous"] = json!(self.previous);
        checkpoint
    }
}
struct ChannelHost {
    store: SessionStore,
    calls: mpsc::Sender<HostCall>,
    cancellation: CancellationToken,
    poll_tool: Option<String>,
    native_tools: std::collections::BTreeSet<String>,
    checkpoint: Arc<tokio::sync::Mutex<TurnCheckpoint>>,
    execution: Arc<codex_provider_auth::ExecutionLease>,
}
impl ChannelHost {
    async fn dispatch(
        &self,
        call: api::ToolCall,
        cancellation: &CancellationToken,
    ) -> api::Result<api::ToolResult> {
        let (reply, receive) = oneshot::channel();
        tokio::select! {
            _ = cancellation.cancelled() => return Err(api::ProviderError::Cancelled),
            sent = self.calls.send(HostCall { call, reply }) => sent.map_err(|_| api::ProviderError::Cancelled)?,
        }
        tokio::select! {
            _ = cancellation.cancelled() => Err(api::ProviderError::Cancelled),
            result = receive => result.map_err(|_| api::ProviderError::Cancelled),
        }
    }
}
impl api::Host for ChannelHost {
    // The lock serializes durable interruption fences with checkpoint writes.
    #[allow(clippy::await_holding_invalid_type)]
    fn execute(
        &self,
        call: api::ToolCall,
        cancellation: CancellationToken,
    ) -> api::ProviderFuture<'_, api::ToolResult> {
        Box::pin(async move {
            // Fence interruption before any operation can run, even if the
            // provider has not emitted its first checkpoint yet. Serialize this
            // with checkpoint writes so a later snapshot cannot clear the fence.
            {
                let mut state = self.checkpoint.lock().await;
                if !state.tools_started {
                    self.execution.mark_tools_started()?;
                    state.tools_started = true;
                    let checkpoint =
                        state.pending(state.latest.clone().unwrap_or_else(|| api::Checkpoint {
                            version: 1,
                            namespace: state.namespace.clone(),
                            context_hash: String::new(),
                            state: json!({}),
                        }));
                    self.store.save(&checkpoint, &cancellation).await?;
                    state.latest = Some(checkpoint);
                }
            }
            let ticket = match self.store.begin_tool(call.clone(), &cancellation).await? {
                ToolDisposition::Replay(result) => return Ok(result),
                ToolDisposition::Execute(ticket) => ticket,
            };
            let mut result = self.dispatch(call.clone(), &cancellation).await?;
            if self.native_tools.contains(&call.name) {
                if result.is_error && result.output["codex_native_structured"]["rejected"] == true {
                    result.output = result.output["codex_native_structured"].clone();
                }
                if !result.is_error {
                    let mut structured = result.output["codex_native_structured"].clone();
                    let mut output = structured["output"].as_str().unwrap_or_default().to_owned();
                    let mut output_exceeded = output.len() > 2 * 1024 * 1024;
                    if output_exceeded {
                        output.clear();
                    }
                    let mut polls = 0;
                    while let Some(session_id) = structured["session_id"].as_i64() {
                        polls += 1;
                        let poll_tool = self.poll_tool.clone().ok_or_else(|| {
                            api::ProviderError::Host(
                                "native shell requires write_stdin continuation".into(),
                            )
                        })?;
                        let polled = self.dispatch(api::ToolCall { id: format!("{}:poll:{polls}", call.id), name: poll_tool, arguments: json!({"session_id":session_id,"chars":"","yield_time_ms":1000,"max_output_tokens":30000}) }, &cancellation).await?;
                        if polled.is_error {
                            result.is_error = true;
                            result.output = polled.output;
                            break;
                        }
                        structured = polled.output["codex_native_structured"].clone();
                        if !output_exceeded {
                            let chunk = structured["output"].as_str().unwrap_or_default();
                            if chunk.len() > 2 * 1024 * 1024 - output.len() {
                                output_exceeded = true;
                                output.clear();
                            } else {
                                output.push_str(chunk);
                            }
                        }
                        // Drain a foreground operation through its exit (or
                        // normal cancellation/timeout) even after the cap. An
                        // early error would leave its child running unnoticed.
                    }
                    if !result.is_error && output_exceeded {
                        result.is_error = true;
                        result.output = json!("native operation output exceeded 2 MiB");
                    } else if !result.is_error {
                        structured["output"] = json!(output);
                        match native_tools::result(&call.name, &call.arguments, &structured) {
                            Ok(output) => result.output = output,
                            Err(error) => {
                                result.is_error = true;
                                result.output = json!({"error":error.to_string()});
                            }
                        }
                    }
                }
            } else if let Some(history) = result.output.get("codex_native_history") {
                result.output = history.clone();
            }
            ticket.finish(result)
        })
    }
    #[allow(clippy::await_holding_invalid_type)]
    fn checkpoint(&self, checkpoint: api::Checkpoint) -> api::ProviderFuture<'_, ()> {
        Box::pin(async move {
            if !checkpoint.state.is_object() {
                return Err(api::ProviderError::Checkpoint(
                    "native checkpoint must be an object".into(),
                ));
            }
            let mut state = self.checkpoint.lock().await;
            let checkpoint = state.pending(checkpoint);
            self.store.save(&checkpoint, &self.cancellation).await?;
            state.latest = Some(checkpoint);
            Ok(())
        })
    }
}

fn call_item(call: api::ToolCall, targets: &BTreeMap<String, ToolTarget>) -> Result<ResponseItem> {
    let target = targets
        .get(&call.name)
        .ok_or_else(|| error("unadvertised native tool"))?;
    let arguments = if let Some(name) = &target.native {
        native_tools::command(name, &call.arguments).map_err(error)?
    } else {
        call.arguments
    };
    let value = if target.custom {
        json!({"type":"custom_tool_call","call_id":call.id,"name":target.name,"namespace":target.namespace,"input":arguments["input"].as_str().ok_or_else(|| error("custom tool requires input string"))?})
    } else {
        json!({"type":"function_call","call_id":call.id,"name":target.name,"namespace":target.namespace,"arguments":serde_json::to_string(&arguments).map_err(error)?})
    };
    serde_json::from_value(value).map_err(error)
}

/// Called after the ordinary tool runtime has executed and persisted its output.
pub(crate) fn tool_result(item: &ResponseItem) -> Result<api::ToolResult> {
    match item {
        ResponseItem::FunctionCallOutput {
            call_id: Some(id),
            output,
            ..
        }
        | ResponseItem::CustomToolCallOutput {
            call_id: id,
            output,
            ..
        } => Ok(api::ToolResult {
            call_id: id.clone(),
            output: serde_json::to_value(&output.body).map_err(error)?,
            is_error: output.success == Some(false),
        }),
        _ => Err(error("native tool did not produce a matching result")),
    }
}

fn message(id: &str, text: String, thinking: bool) -> ResponseItem {
    if thinking {
        ResponseItem::Reasoning {
            id: Some(ResponseItemId::from_server(id.to_owned())),
            summary: vec![
                codex_protocol::models::ReasoningItemReasoningSummary::SummaryText { text },
            ],
            content: None,
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
        }
    } else {
        ResponseItem::Message {
            encrypted_content: None,
            status: None,
            id: Some(ResponseItemId::from_server(id.to_owned())),
            role: "assistant".into(),
            content: vec![ContentItem::OutputText {
                text,
                annotations: None,
                logprobs: None,
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }
}

async fn emit(tx: &mpsc::Sender<Result<ResponseEvent>>, event: ResponseEvent) -> Result<()> {
    tx.send(Ok(event)).await.map_err(|_| CodexErr::TurnAborted)
}

async fn finish_item(
    tx: &mpsc::Sender<Result<ResponseEvent>>,
    active: &mut Option<(String, String, bool)>,
    history: &mut Vec<api::Message>,
) -> Result<()> {
    if let Some((id, text, thinking)) = active.take() {
        let item = message(&id, text, thinking);
        for entry in messages(std::slice::from_ref(&item))? {
            push(history, entry.role, entry.content);
        }
        emit(tx, ResponseEvent::OutputItemDone(item)).await?;
    }
    Ok(())
}

fn lineage_hash(system: &str, history: &[api::Message]) -> Result<String> {
    Ok(codex_provider_auth::digest(
        &serde_json::to_vec(&json!({"system":system,"messages":history})).map_err(error)?,
    ))
}

fn failed_result_bindings(history: &[api::Message]) -> Result<BTreeMap<String, String>> {
    history
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            api::Content::ToolResult(result) if result.is_error => Some(result),
            _ => None,
        })
        .map(|result| {
            Ok((
                result.call_id.clone(),
                codex_provider_auth::digest(&serde_json::to_vec(&result.output).map_err(error)?),
            ))
        })
        .collect()
}

fn validate_lineage(request: &mut api::Request) -> Result<()> {
    if request.checkpoint.as_ref().is_some_and(|checkpoint| {
        checkpoint.version != 1 || checkpoint.namespace != request.namespace
    }) {
        return Err(error("native checkpoint identity or version mismatch"));
    }
    if let Some(checkpoint) = request.checkpoint.as_ref()
        && checkpoint.state["codex_host_pending"] == true
    {
        if checkpoint.state["codex_host_tools_started"] != false {
            return Err(error(
                "native generation was interrupted after tool execution started; inspect its effects and explicitly branch the thread before continuing",
            ));
        }
        // No host operation started. Retain the last completed signed transcript
        // and append the ordinary persisted interruption/new-user history.
        let previous = checkpoint
            .state
            .get("codex_host_previous")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        request.checkpoint = if request.namespace.provider == "cursor" {
            // Cursor sends only the newest user message when resuming its native
            // conversation. Rebuild from public history to retain the cancelled
            // prompt and partial response, which are absent from the old state.
            None
        } else {
            serde_json::from_value(previous).map_err(error)?
        };
    }
    let Some(checkpoint) = &request.checkpoint else {
        return Ok(());
    };
    if checkpoint.state["codex_host_pending"] != false {
        return Err(error(
            "native checkpoint has an incomplete or unverified host history; refusing automatic replay",
        ));
    }
    let lineage = &checkpoint.state["codex_host_lineage"];
    let length = lineage["length"]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| error("missing native checkpoint history binding"))?;
    // ResponseItem's failure flag is not serialized in ordinary rollout history.
    // Restore it only for the same call ID and exact saved output bytes.
    if let Some(failed) = lineage["failed_results"].as_object() {
        for message in request.messages.iter_mut().take(length) {
            for content in &mut message.content {
                if let api::Content::ToolResult(result) = content
                    && let Some(hash) = failed
                        .get(&result.call_id)
                        .and_then(serde_json::Value::as_str)
                    && hash
                        == codex_provider_auth::digest(
                            &serde_json::to_vec(&result.output).map_err(error)?,
                        )
                {
                    result.is_error = true;
                }
            }
        }
    }
    if length > request.messages.len()
        || lineage["hash"].as_str()
            != Some(&lineage_hash(&request.system, &request.messages[..length])?)
    {
        return Err(error(
            "native checkpoint history changed; refusing to resume stale upstream state",
        ));
    }
    request.metadata["host_checkpoint_prefix"] = json!(length);
    Ok(())
}

pub(crate) async fn stream(
    provider: SharedModelProvider,
    prompt: &Prompt,
    model: &ModelInfo,
    thread: String,
    reasoning_effort: Option<String>,
    checkpoint_scope: String,
    execution_purpose: &str,
) -> Result<ResponseStream> {
    let cancellation = CancellationToken::new();
    let credentials = provider
        .native_credentials(cancellation.clone())
        .await
        .map_err(error)?;
    let namespace = api::Namespace {
        provider: provider
            .info()
            .wire_api
            .native_provider_id()
            .ok_or_else(|| error("missing native provider ID"))?
            .into(),
        account: credentials.account_id.clone(),
        model: model.slug.clone(),
        thread,
    };
    let home = provider
        .native_state_home()
        .ok_or_else(|| error("native provider requires local state storage"))?;
    let store = SessionStore::new(&home, namespace.clone()).with_checkpoint_scope(checkpoint_scope);
    let execution = Arc::new(
        store
            .begin_execution(execution_purpose, &cancellation)
            .await
            .map_err(error)?,
    );
    let (mut definitions, mut targets) = tools(prompt)?;
    let poll_tool = targets
        .iter()
        .find(|(_, target)| target.name == "write_stdin")
        .map(|(name, _)| name.clone());
    if native_operations_available()
        && namespace.provider == "cursor"
        && poll_tool.is_some()
        && let Some(executor) = targets
            .values()
            .find(|target| target.name == "exec_command" && !target.custom)
            .cloned()
    {
        for definition in native_tools::definitions() {
            let mut target = executor.clone();
            target.native = Some(definition.name.clone());
            if targets.insert(definition.name.clone(), target).is_some() {
                return Err(error("reserved Cursor native tool name collision"));
            }
            definitions.push(definition);
        }
    }
    let mut request = api::Request {
        namespace,
        system: prompt.base_instructions.text.clone(),
        messages: messages(&prompt.input)?,
        tools: definitions,
        parallel_tool_calls: prompt.parallel_tool_calls,
        max_output_tokens: None,
        reasoning_effort,
        output_schema: prompt.output_schema.clone(),
        checkpoint: store.load().map_err(error)?,
        metadata: json!({}),
    };
    validate_lineage(&mut request)?;
    let mut history = request.messages.clone();
    let system = request.system.clone();
    let final_store = store.clone();
    let checkpoint_state = Arc::new(tokio::sync::Mutex::new(TurnCheckpoint {
        namespace: request.namespace.clone(),
        latest: None,
        previous: request.checkpoint.clone(),
        tools_started: false,
    }));
    let backend = provider.native_provider().map_err(error)?;
    let session = backend
        .start(
            request.clone(),
            api::Context {
                credentials,
                cancellation: cancellation.child_token(),
            },
        )
        .await
        .map_err(error)?;
    let (calls, mut host_calls) = mpsc::channel::<HostCall>(128);
    let native_tools = targets
        .iter()
        .filter(|(_, target)| target.native.is_some())
        .map(|(name, _)| name.clone())
        .collect();
    let host = Arc::new(ChannelHost {
        store,
        calls,
        cancellation: cancellation.clone(),
        poll_tool,
        native_tools,
        checkpoint: checkpoint_state.clone(),
        execution: execution.clone(),
    });
    let (events, mut provider_events) = mpsc::channel(128);
    let driver_cancel = cancellation.clone();
    let error_events = events.clone();
    tokio::spawn(async move {
        if let Err(error) = api::drive_session(session, host, &request, driver_cancel, events).await
        {
            let _ = error_events.send(Err(error)).await;
        }
    });
    let (tx, rx_event) = mpsc::channel(128);
    let (result_tx, mut results) = mpsc::channel::<api::ToolResult>(128);
    let consumer_dropped = cancellation.clone();
    tokio::spawn(async move {
        let result = async {
            let mut replies = BTreeMap::new();
            let mut active = None;
            let mut counter = 0u64;
            let response_id = uuid::Uuid::new_v4();
            let mut usage = None;
            emit(&tx, ResponseEvent::Created { response_id: None }).await?;
            loop {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(CodexErr::TurnAborted),
                    result = results.recv() => {
                        let result = result.ok_or(CodexErr::TurnAborted)?;
                        let reply: oneshot::Sender<api::ToolResult> = replies.remove(&result.call_id).ok_or_else(|| error("unexpected native tool result ID"))?;
                        let mut history_result = result.clone();
                        if let Some(output) = result.output.get("codex_native_history") { history_result.output = output.clone(); }
                        push(&mut history, api::Role::Tool, vec![api::Content::ToolResult(history_result)]);
                        let _ = reply.send(result);
                    }
                    event = provider_events.recv() => {
                        match event.ok_or_else(|| error("native stream closed without completion"))?.map_err(error)? {
                            event @ (api::Event::TextDelta(_) | api::Event::ThinkingDelta(_)) => {
                                let thinking = matches!(event, api::Event::ThinkingDelta(_));
                                let text = match event { api::Event::TextDelta(text) | api::Event::ThinkingDelta(text) => text, _ => unreachable!() };
                                if active.as_ref().is_some_and(|(_, _, kind)| *kind != thinking) { finish_item(&tx, &mut active, &mut history).await?; }
                                if active.is_none() {
                                    counter += 1;
                                    let id = format!("native-{response_id}-{counter}");
                                    emit(&tx, ResponseEvent::OutputItemAdded(message(&id, String::new(), thinking))).await?;
                                    active = Some((id, String::new(), thinking));
                                }
                                active.as_mut().ok_or_else(|| error("native message state missing"))?.1.push_str(&text);
                                emit(&tx, if thinking { ResponseEvent::ReasoningSummaryDelta { delta: text, summary_index: 0 } } else { ResponseEvent::OutputTextDelta(text) }).await?;
                            }
                            api::Event::ToolCall(_) => { finish_item(&tx, &mut active, &mut history).await?; }
                            api::Event::Checkpoint(_) => {}
                            api::Event::Usage(value) => { usage = Some(TokenUsage { input_tokens: value.input_tokens.min(i64::MAX as u64) as i64, cached_input_tokens: value.cached_input_tokens.min(i64::MAX as u64) as i64, output_tokens: value.output_tokens.min(i64::MAX as u64) as i64, reasoning_output_tokens: value.reasoning_tokens.min(i64::MAX as u64) as i64, total_tokens: value.input_tokens.saturating_add(value.output_tokens).min(i64::MAX as u64) as i64, ..TokenUsage::default() }); }
                            api::Event::Completed { id } => {
                                finish_item(&tx, &mut active, &mut history).await?;
                                let checkpoint = checkpoint_state.lock().await.latest.clone();
                                if let Some(mut checkpoint) = checkpoint {
                                    checkpoint.state["codex_host_pending"] = json!(false);
                                    if let Some(state) = checkpoint.state.as_object_mut() { state.remove("codex_host_previous"); }
                                    checkpoint.state["codex_host_lineage"] = json!({"length":history.len(),"hash":lineage_hash(&system, &history)?,"failed_results":failed_result_bindings(&history)?});
                                    final_store.save(&checkpoint, &cancellation).await.map_err(error)?;
                                }
                                execution.complete().map_err(error)?;
                                emit(&tx, ResponseEvent::Completed { response_id: id, token_usage: usage, usage_metadata: None, end_turn: Some(true) }).await?;
                                return Ok(());
                            }
                        }
                    }
                    call = host_calls.recv(), if !host_calls.is_closed() => {
                        if let Some(call) = call {
                            finish_item(&tx, &mut active, &mut history).await?;
                            let id = call.call.id.clone();
                            let item = match call_item(call.call, &targets) {
                                Ok(item) => item,
                                Err(error) => {
                                    let _ = call.reply.send(api::ToolResult { call_id: id, output: json!({"error":error.to_string()}), is_error: true });
                                    continue;
                                }
                            };
                            for entry in messages(std::slice::from_ref(&item))? { push(&mut history, entry.role, entry.content); }
                            replies.insert(id, call.reply);
                            emit(&tx, ResponseEvent::OutputItemDone(item)).await?;
                        }
                    }
                }
            }
        }.await;
        if let Err(error) = result {
            let _ = tx.send(Err(error)).await;
        }
        cancellation.cancel();
    });
    Ok(ResponseStream {
        native_tool_results: Some(result_tx),
        rx_event,
        interrupt: None,
        consumer_dropped,
    })
}

#[cfg(test)]
#[path = "native_provider_tests.rs"]
mod tests;
