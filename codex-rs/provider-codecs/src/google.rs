use crate::WireCodec;
use crate::WireRequest;
use crate::output_text;
use crate::protocol;
use crate::string;
use codex_provider_api::*;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;

pub struct GoogleCodec {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub project: String,
    parts: Vec<Value>,
    calls: Vec<ToolCall>,
    declared: Vec<String>,
    id: String,
    done: bool,
}
impl GoogleCodec {
    pub fn new(
        url: String,
        headers: BTreeMap<String, String>,
        project: String,
        tools: &[ToolDefinition],
    ) -> Self {
        Self {
            url,
            headers,
            project,
            parts: Vec::new(),
            calls: Vec::new(),
            declared: tools.iter().map(|tool| tool.name.clone()).collect(),
            id: String::new(),
            done: false,
        }
    }
    fn message_parts(
        &self,
        message: &Message,
        request: &Request,
        names: &BTreeMap<String, String>,
    ) -> Result<Vec<Value>> {
        for block in &message.content {
            if let Content::Opaque { namespace, value } = block {
                if namespace != &request.namespace {
                    return Err(ProviderError::Checkpoint(
                        "Antigravity replay namespace mismatch".into(),
                    ));
                }
                let parts = value["google_parts"].as_array().ok_or_else(|| {
                    ProviderError::Checkpoint("foreign Antigravity replay format".into())
                })?;
                // Claude streams unsigned, empty text delimiters. They are not
                // content blocks and its upstream translator rejects replay.
                return Ok(parts
                    .iter()
                    .filter(|part| {
                        !(part.as_object().is_some_and(|object| object.len() == 1)
                            && part["text"].as_str() == Some(""))
                    })
                    .cloned()
                    .collect());
            }
        }
        let mut parts = Vec::new();
        for block in &message.content {
            match block {
                Content::Text { text } => parts.push(json!({"text":text})),
                Content::Image { url } => {
                    if let Some(data) = url.strip_prefix("data:") {
                        let (mime, data) = data.split_once(";base64,").ok_or_else(|| {
                            protocol("Antigravity image must be a base64 data URL")
                        })?;
                        parts.push(json!({"inlineData":{"mimeType":mime,"data":data}}));
                    } else {
                        return Err(ProviderError::Unsupported(
                            "Antigravity images require inline data".into(),
                        ));
                    }
                }
                Content::Thinking {
                    text,
                    signature: Some(signature),
                } => parts.push(json!({"text":text,"thought":true,"thoughtSignature":signature})),
                Content::Thinking { .. } => {}
                Content::ToolCall(call) => {
                    let mut part = json!({"functionCall":{"name":call.name,"args":call.arguments}});
                    if request.namespace.model.starts_with("claude-")
                        || request.namespace.model.starts_with("gpt-oss-")
                    {
                        part["functionCall"]["id"] = json!(call.id);
                    }
                    parts.push(part);
                }
                Content::ToolResult(result) => {
                    let name = names.get(&result.call_id).ok_or_else(|| {
                        protocol("Antigravity tool result has no matching tool call")
                    })?;
                    let response = if result.is_error {
                        json!({"error":output_text(&result.output)})
                    } else {
                        json!({"output":output_text(&result.output)})
                    };
                    let mut part = json!({"functionResponse":{"name":name,"response":response}});
                    if request.namespace.model.starts_with("claude-")
                        || request.namespace.model.starts_with("gpt-oss-")
                    {
                        part["functionResponse"]["id"] = json!(result.call_id);
                    }
                    parts.push(part);
                }
                Content::Opaque { .. } => {}
            }
        }
        Ok(parts)
    }
}
/// Preserve supported constraints; reject reference/combinator schemas rather than
/// quietly changing what a tool accepts. `additionalProperties: false` is a host
/// validation constraint and has no equivalent in Google's function declaration schema.
fn tool_schema(value: &Value) -> Result<Value> {
    match value {
        Value::Object(object) => {
            let mut result = serde_json::Map::new();
            for (key, value) in object {
                if key == "$schema"
                    || (key == "additionalProperties" && value == &Value::Bool(false))
                {
                    continue;
                }
                if [
                    "$ref",
                    "$defs",
                    "oneOf",
                    "allOf",
                    "not",
                    "patternProperties",
                    "additionalProperties",
                ]
                .contains(&key.as_str())
                {
                    return Err(ProviderError::Unsupported(format!(
                        "Antigravity tool schema keyword {key}"
                    )));
                }
                let value = if key == "properties" {
                    let properties = value.as_object().ok_or_else(|| {
                        protocol("Antigravity schema properties must be an object")
                    })?;
                    let properties = properties
                        .iter()
                        .map(|(name, schema)| Ok((name.clone(), tool_schema(schema)?)))
                        .collect::<Result<serde_json::Map<String, Value>>>()?;
                    Value::Object(properties)
                } else if matches!(key.as_str(), "enum" | "default" | "examples" | "const") {
                    value.clone()
                } else {
                    tool_schema(value)?
                };
                result.insert(key.clone(), value);
            }
            Ok(Value::Object(result))
        }
        Value::Array(array) => Ok(Value::Array(
            array.iter().map(tool_schema).collect::<Result<_>>()?,
        )),
        value => Ok(value.clone()),
    }
}
impl WireCodec for GoogleCodec {
    fn request(&self, request: &Request, context: &Context) -> Result<WireRequest> {
        let mut headers = self.headers.clone();
        headers.insert(
            "authorization".into(),
            format!("Bearer {}", context.credentials.access_token),
        );
        let mut names = BTreeMap::new();
        for message in &request.messages {
            for block in &message.content {
                if let Content::ToolCall(call) = block {
                    names.insert(call.id.clone(), call.name.clone());
                }
            }
        }
        let mut contents: Vec<Value> = Vec::new();
        let mut system = Vec::new();
        if !request.system.is_empty() {
            system.push(json!({"text":request.system}));
        }
        for message in &request.messages {
            let parts = self.message_parts(message, request, &names)?;
            if parts.is_empty() {
                continue;
            }
            if message.role == Role::System {
                system.extend(parts);
                continue;
            }
            let role = if message.role == Role::Assistant {
                "model"
            } else {
                "user"
            };
            if let Some(previous) = contents.last_mut().filter(|value| value["role"] == role) {
                previous["parts"]
                    .as_array_mut()
                    .ok_or_else(|| protocol("invalid Antigravity history"))?
                    .extend(parts);
            } else {
                contents.push(json!({"role":role,"parts":parts}));
            }
        }
        let mut inner = json!({"contents":contents,"sessionId":request.namespace.thread});
        if !system.is_empty() {
            inner["systemInstruction"] = json!({"parts":system});
        }
        if !request.tools.is_empty() {
            let declarations = request.tools.iter().map(|tool| Ok(json!({"name":tool.name,"description":tool.description,"parameters":tool_schema(&tool.parameters)?}))).collect::<Result<Vec<_>>>()?;
            inner["tools"] = json!([{"functionDeclarations":declarations}]);
            inner["toolConfig"] = json!({"functionCallingConfig":{"mode":if request.namespace.model.to_lowercase().contains("claude") {"VALIDATED"} else {"AUTO"}}});
        }
        if let Some(max) = request.max_output_tokens {
            inner["generationConfig"]["maxOutputTokens"] = json!(max);
        }
        if let Some(effort) = &request.reasoning_effort {
            if request.namespace.model.starts_with("gemini-") && effort != "none" && effort != "off"
            {
                let level = match effort.as_str() {
                    "low" => "LOW",
                    "medium" => "MEDIUM",
                    "high" | "xhigh" | "max" => "HIGH",
                    _ => {
                        return Err(ProviderError::Unsupported(
                            "Antigravity reasoning effort".into(),
                        ));
                    }
                };
                inner["generationConfig"]["thinkingConfig"] =
                    json!({"thinkingLevel":level,"includeThoughts":true});
            } else if effort != "none" && effort != "off" {
                return Err(ProviderError::Unsupported(
                    "Claude Antigravity reasoning uses discovered model variants".into(),
                ));
            }
        }
        if let Some(schema) = &request.output_schema {
            inner["generationConfig"]["responseMimeType"] = json!("application/json");
            inner["generationConfig"]["responseJsonSchema"] = schema.clone();
        }
        let body = json!({"model":request.namespace.model,"project":self.project,"userAgent":"antigravity","requestType":"agent","requestId":format!("agent-{}",uuid::Uuid::new_v4()),"request":inner});
        Ok(WireRequest {
            url: self.url.clone(),
            headers,
            body,
        })
    }
    fn reset(&mut self) {
        self.parts.clear();
        self.calls.clear();
        self.id.clear();
        self.done = false;
    }
    fn frame(&mut self, _event: &str, data: &str) -> Result<Vec<Event>> {
        let outer: Value =
            serde_json::from_str(data).map_err(|_| protocol("invalid Antigravity SSE JSON"))?;
        if outer.get("error").is_some() {
            return Err(protocol("Antigravity upstream error"));
        }
        let value = outer
            .get("response")
            .filter(|value| value.is_object())
            .ok_or_else(|| protocol("Antigravity response wrapper missing"))?;
        let mut events = Vec::new();
        if let Some(id) = value["responseId"].as_str() {
            self.id = id.to_owned();
        }
        if let Some(usage) = value.get("usageMetadata") {
            events.push(Event::Usage(Usage {
                input_tokens: usage["promptTokenCount"].as_u64().unwrap_or(0),
                output_tokens: usage["candidatesTokenCount"].as_u64().unwrap_or(0),
                cached_input_tokens: usage["cachedContentTokenCount"].as_u64().unwrap_or(0),
                reasoning_tokens: usage["thoughtsTokenCount"].as_u64().unwrap_or(0),
            }));
        }
        let Some(candidates) = value["candidates"].as_array() else {
            if value.get("usageMetadata").is_some() {
                return Ok(events);
            }
            return Err(protocol("Antigravity candidates missing"));
        };
        if candidates.len() > 1 {
            return Err(protocol("multiple Antigravity candidates are unsupported"));
        }
        for candidate in candidates {
            if let Some(parts) = candidate["content"]["parts"].as_array() {
                for part in parts {
                    if !part.is_object() {
                        return Err(protocol("invalid Antigravity part"));
                    }
                    if let Some(text) = part["text"].as_str() {
                        events.push(if part["thought"] == true {
                            Event::ThinkingDelta(text.to_owned())
                        } else {
                            Event::TextDelta(text.to_owned())
                        });
                    }
                    if let Some(call) = part.get("functionCall") {
                        let name = string(call, "name")?;
                        if !self.declared.contains(&name) {
                            return Err(protocol("Antigravity returned undeclared tool"));
                        }
                        let arguments = call.get("args").cloned().unwrap_or_else(|| json!({}));
                        if !arguments.is_object() {
                            return Err(protocol(
                                "Antigravity function arguments must be an object",
                            ));
                        }
                        self.calls.push(ToolCall {
                            id: call["id"]
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| format!("agy-{}", uuid::Uuid::new_v4())),
                            name,
                            arguments,
                        });
                    }
                    self.parts.push(part.clone());
                }
            }
            if let Some(reason) = candidate["finishReason"].as_str() {
                match reason {
                    "STOP" | "MAX_TOKENS" => self.done = true,
                    _ => return Err(protocol(format!("Antigravity finish reason {reason}"))),
                }
            }
        }
        Ok(events)
    }
    fn finished(&self) -> bool {
        self.done
    }
    fn terminal_grace(&self) -> std::time::Duration {
        std::time::Duration::from_millis(500)
    }
    fn response_id(&self) -> String {
        self.id.clone()
    }
    fn assistant(&self, namespace: &Namespace) -> Result<Message> {
        let mut content = Vec::new();
        for part in &self.parts {
            if let Some(text) = part["text"].as_str() {
                content.push(if part["thought"] == true {
                    Content::Thinking {
                        text: text.to_owned(),
                        signature: part["thoughtSignature"]
                            .as_str()
                            .or_else(|| part["thought_signature"].as_str())
                            .map(str::to_owned),
                    }
                } else {
                    Content::Text {
                        text: text.to_owned(),
                    }
                });
            }
        }
        content.extend(self.calls.iter().cloned().map(Content::ToolCall));
        content.push(Content::Opaque {
            namespace: namespace.clone(),
            value: json!({"google_parts":self.parts}),
        });
        Ok(Message {
            role: Role::Assistant,
            content,
        })
    }
}
