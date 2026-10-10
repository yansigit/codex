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
        request: Request,
        context: Context,
    ) -> ProviderFuture<'_, Box<dyn ProviderSession>> {
        Box::pin(async move {
            if request.namespace.provider != self.id() {
                return Err(ProviderError::Protocol(
                    "Claude provider namespace mismatch".into(),
                ));
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
