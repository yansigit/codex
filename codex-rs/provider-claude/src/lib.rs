//! Direct Claude Pro/Max OAuth Messages transport. OAuth lifecycle lives in shared auth.
use codex_provider_api::*;
use codex_provider_codecs::*;
use reqwest::Client;
use std::collections::BTreeMap;

pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const REDIRECT_URI: &str = "http://localhost:53692/callback";
pub const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

pub struct ClaudeProvider {
    client: Client,
    base_url: String,
}
impl Default for ClaudeProvider {
    fn default() -> Self {
        Self::new(Client::new())
    }
}
impl ClaudeProvider {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            base_url: "https://api.anthropic.com".into(),
        }
    }
    pub fn with_base_url(client: Client, base_url: String) -> Result<Self> {
        let url = url::Url::parse(&base_url)
            .map_err(|_| ProviderError::Protocol("invalid Claude endpoint".into()))?;
        if url.scheme() != "https"
            && !(url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1")))
        {
            return Err(ProviderError::Protocol(
                "Claude endpoint must use HTTPS".into(),
            ));
        }
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').into(),
        })
    }
    fn headers() -> BTreeMap<String, String> {
        BTreeMap::from([("anthropic-beta".into(),"claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14".into()),("user-agent".into(),"claude-cli/2.1.280".into()),("x-app".into(),"cli".into())])
    }
}
impl Provider for ClaudeProvider {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn models(&self, context: Context) -> ProviderFuture<'_, Vec<Model>> {
        Box::pin(async move {
            let mut models = Vec::new();
            let mut after = None::<String>;
            loop {
                let mut builder = self
                    .client
                    .get(format!("{}/v1/models", self.base_url))
                    .bearer_auth(&context.credentials.access_token)
                    .header("anthropic-version", "2023-06-01")
                    .query(&[("limit", "100")]);
                for (key, value) in Self::headers() {
                    builder = builder.header(key, value);
                }
                if let Some(after) = &after {
                    builder = builder.query(&[("after_id", after)]);
                }
                let response = tokio::select! { biased; _=context.cancellation.cancelled()=>return Err(ProviderError::Cancelled),response=builder.send()=>response.map_err(|_|ProviderError::Transport("Claude model discovery failed".into()))?};
                let body = bounded_json(response, &context).await?;
                let data = body["data"]
                    .as_array()
                    .ok_or_else(|| ProviderError::Protocol("Claude models data missing".into()))?;
                for model in data {
                    let id = model["id"]
                        .as_str()
                        .ok_or_else(|| ProviderError::Protocol("Claude model id missing".into()))?;
                    models.push(Model {
                        id: id.into(),
                        name: model["display_name"].as_str().unwrap_or(id).into(),
                        context_window: None,
                    });
                    if models.len() > 500 {
                        return Err(ProviderError::Protocol(
                            "Claude model catalog exceeds 500 models".into(),
                        ));
                    }
                }
                if body["has_more"] != true {
                    break;
                }
                let cursor = body["last_id"].as_str().ok_or_else(|| {
                    ProviderError::Protocol("Claude pagination cursor missing".into())
                })?;
                if after.as_deref() == Some(cursor) {
                    return Err(ProviderError::Protocol(
                        "Claude repeated pagination cursor".into(),
                    ));
                }
                after = Some(cursor.into());
            }
            Ok(models)
        })
    }
    fn start(
        &self,
        mut request: Request,
        context: Context,
    ) -> ProviderFuture<'_, Box<dyn ProviderSession>> {
        Box::pin(async move {
            if request.namespace.provider != self.id() {
                return Err(ProviderError::Protocol(
                    "Claude provider namespace mismatch".into(),
                ));
            }
            if request
                .reasoning_effort
                .as_deref()
                .is_some_and(|effort| effort != "none" && effort != "off")
            {
                let mut url = url::Url::parse(&format!("{}/v1/models/", self.base_url))
                    .map_err(|_| ProviderError::Protocol("invalid Claude model endpoint".into()))?;
                url.path_segments_mut()
                    .map_err(|_| ProviderError::Protocol("invalid Claude model endpoint".into()))?
                    .pop_if_empty()
                    .push(&request.namespace.model);
                let mut builder = self
                    .client
                    .get(url)
                    .bearer_auth(&context.credentials.access_token)
                    .header("anthropic-version", "2023-06-01");
                for (key, value) in Self::headers() {
                    builder = builder.header(key, value);
                }
                let response = tokio::select! { biased; _ = context.cancellation.cancelled() => return Err(ProviderError::Cancelled),
                response = tokio::time::timeout(std::time::Duration::from_secs(30), builder.send()) => response.map_err(|_| ProviderError::Transport("Claude capability discovery timeout".into()))?.map_err(|_| ProviderError::Transport("Claude capability discovery failed".into()))? };
                let model = bounded_json(response, &context).await?;
                request.metadata["anthropic_adaptive_thinking"] = serde_json::json!(
                    model["capabilities"]["thinking"]["types"]["adaptive"]["supported"] == true
                );
                if let Some(effort) = request.reasoning_effort.as_ref() {
                    if request.metadata["anthropic_adaptive_thinking"] == true
                        && model["capabilities"]["effort"][effort]["supported"] != true
                    {
                        return Err(ProviderError::Unsupported(format!(
                            "Claude model does not support reasoning effort {effort}"
                        )));
                    }
                }
            }
            let codec = MessagesCodec::new(
                format!("{}/v1/messages", self.base_url),
                Self::headers(),
                true,
                &request.tools,
            )?;
            Ok(Box::new(
                HttpSession::start(self.client.clone(), Box::new(codec), request, context).await?,
            ) as Box<dyn ProviderSession>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    #[tokio::test]
    async fn adaptive_thinking_uses_discovered_model_capability() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/v1/models/claude-test")).and(header("authorization","Bearer fixture-token")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"capabilities":{"thinking":{"types":{"adaptive":{"supported":true}}},"effort":{"high":{"supported":true}}}}))).expect(1).mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(body_partial_json(
                json!({"thinking":{"type":"adaptive"},"output_config":{"effort":"high"}}),
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let provider = ClaudeProvider::with_base_url(Client::new(), server.uri()).unwrap();
        let context = Context {
            credentials: Credentials {
                access_token: "fixture-token".into(),
                account_id: "account-a".into(),
                metadata: json!({}),
            },
            cancellation: Default::default(),
        };
        let request = Request {
            namespace: Namespace {
                provider: "claude".into(),
                account: "account-a".into(),
                model: "claude-test".into(),
                thread: "thread-a".into(),
            },
            system: String::new(),
            messages: vec![Message {
                role: Role::User,
                content: vec![Content::Text {
                    text: "hello".into(),
                }],
            }],
            tools: vec![],
            parallel_tool_calls: true,
            max_output_tokens: Some(128),
            reasoning_effort: Some("high".into()),
            output_schema: None,
            checkpoint: None,
            metadata: json!({}),
        };
        drop(provider.start(request, context).await.unwrap());
        server.verify().await;
    }
}
