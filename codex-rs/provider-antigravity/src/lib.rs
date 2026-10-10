//! Native Cloud Code Assist transport for Antigravity. Project onboarding is an RPC,
//! not a delegated `agy` process. All signed parts stay with their exact assistant turn.
use codex_provider_api::*;
use codex_provider_codecs::*;
use reqwest::Client;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::Mutex;

pub const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const CLIENT_ID: &str =
    "REMOVED_OAUTH_CLIENT_ID";
pub const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
pub const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

pub struct AntigravityProvider {
    client: Client,
    base_url: String,
    projects: Mutex<BTreeMap<String, String>>,
}
impl Default for AntigravityProvider {
    fn default() -> Self {
        Self::new(Client::new())
    }
}
impl AntigravityProvider {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            base_url: "https://daily-cloudcode-pa.googleapis.com".into(),
            projects: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn with_base_url(client: Client, base_url: String) -> Result<Self> {
        let url = url::Url::parse(&base_url)
            .map_err(|_| ProviderError::Protocol("invalid Antigravity endpoint".into()))?;
        if url.scheme() != "https"
            && !(url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1")))
        {
            return Err(ProviderError::Protocol(
                "Antigravity endpoint must use HTTPS".into(),
            ));
        }
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').into(),
            projects: Mutex::new(BTreeMap::new()),
        })
    }
    async fn rpc(&self, name: &str, body: Value, context: &Context) -> Result<Value> {
        let request = self
            .client
            .post(format!("{}/v1internal:{name}", self.base_url))
            .bearer_auth(&context.credentials.access_token)
            .header("user-agent", "antigravity-hub/2.2.1")
            .json(&body);
        self.read_request(request, context).await
    }
    async fn read_request(
        &self,
        request: reqwest::RequestBuilder,
        context: &Context,
    ) -> Result<Value> {
        let response = tokio::select! {biased;_=context.cancellation.cancelled()=>return Err(ProviderError::Cancelled),response=tokio::time::timeout(Duration::from_secs(30),request.send())=>response.map_err(|_|ProviderError::Transport("Antigravity RPC timed out".into()))?.map_err(|_|ProviderError::Transport("Antigravity RPC failed".into()))?};
        bounded_json(response, context).await
    }
    /// Resolve an account project and expose it for shared credential metadata persistence.
    pub async fn discover_project(&self, context: &Context) -> Result<String> {
        if let Some(project) = context.credentials.metadata["project_id"]
            .as_str()
            .filter(|project| !project.is_empty())
        {
            return Ok(project.to_owned());
        }
        let mut projects = tokio::select! {biased;_=context.cancellation.cancelled()=>return Err(ProviderError::Cancelled),projects=self.projects.lock()=>projects};
        if let Some(project) = projects.get(&context.credentials.account_id) {
            return Ok(project.clone());
        }
        let load = self
            .rpc(
                "loadCodeAssist",
                json!({"metadata":{"ideType":"ANTIGRAVITY"}}),
                context,
            )
            .await?;
        let project = if let Some(project) = extract_project(&load) {
            project
        } else {
            let tier = load["allowedTiers"]
                .as_array()
                .and_then(|tiers| tiers.iter().find(|tier| tier["isDefault"] == true))
                .and_then(|tier| tier["id"].as_str())
                .or_else(|| load["currentTier"]["id"].as_str())
                .unwrap_or("free-tier");
            let request=self.client.post(format!("{}/v1internal:onboardUser",self.base_url)).bearer_auth(&context.credentials.access_token).header("user-agent","google-api-nodejs-client/10.3.0").header("x-goog-api-client","gl-node/22.21.1").json(&json!({"tier_id":tier,"metadata":{"ide_type":"ANTIGRAVITY","ide_name":"antigravity","ide_version":"2.2.1"}}));
            let mut operation = self.read_request(request, context).await?;
            let end = tokio::time::Instant::now() + Duration::from_secs(300);
            loop {
                if operation.get("error").is_some() {
                    return Err(ProviderError::Authentication(
                        "Antigravity onboarding rejected".into(),
                    ));
                }
                if operation["done"] == true {
                    break extract_project(&operation["response"])
                        .or_else(|| extract_project(&operation))
                        .ok_or_else(|| {
                            ProviderError::Protocol("onboarding completed without a project".into())
                        })?;
                }
                let name = operation["name"]
                    .as_str()
                    .ok_or_else(|| {
                        ProviderError::Protocol("onboarding operation name missing".into())
                    })?
                    .to_owned();
                if !name.starts_with("operations/")
                    || name.contains("..")
                    || name.contains('?')
                    || name.contains('#')
                {
                    return Err(ProviderError::Protocol(
                        "invalid onboarding operation name".into(),
                    ));
                }
                if tokio::time::Instant::now() >= end {
                    return Err(ProviderError::Transport(
                        "Antigravity onboarding timeout".into(),
                    ));
                }
                tokio::select! {biased;_=context.cancellation.cancelled()=>return Err(ProviderError::Cancelled),_=tokio::time::sleep(Duration::from_secs(5))=>{}};
                // Poll the single operation. Never repeat the onboarding POST.
                operation = self
                    .read_request(
                        self.client
                            .get(format!("{}/v1internal/{name}", self.base_url))
                            .bearer_auth(&context.credentials.access_token)
                            .header("user-agent", "google-api-nodejs-client/10.3.0")
                            .header("x-goog-api-client", "gl-node/22.21.1"),
                        context,
                    )
                    .await?;
            }
        };
        projects.insert(context.credentials.account_id.clone(), project.clone());
        Ok(project)
    }
}
fn extract_project(value: &Value) -> Option<String> {
    [
        "cloudaicompanionProject",
        "companionProject",
        "projectId",
        "project",
    ]
    .iter()
    .find_map(|key| {
        value[*key]
            .as_str()
            .or_else(|| value[*key]["id"].as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    })
}
impl Provider for AntigravityProvider {
    fn id(&self) -> &'static str {
        "antigravity"
    }
    fn models(&self, context: Context) -> ProviderFuture<'_, Vec<Model>> {
        Box::pin(async move {
            self.discover_project(&context).await?;
            let body = self
                .rpc("fetchAvailableModels", json!({}), &context)
                .await?;
            let catalog = body["models"].as_object().ok_or_else(|| {
                ProviderError::Protocol("Antigravity catalog models missing".into())
            })?;
            if catalog.len() > 500 {
                return Err(ProviderError::Protocol(
                    "Antigravity catalog exceeds 500 models".into(),
                ));
            }
            Ok(catalog
                .iter()
                .map(|(id, model)| Model {
                    id: id.clone(),
                    name: model["displayName"]
                        .as_str()
                        .or_else(|| model["name"].as_str())
                        .unwrap_or(id)
                        .into(),
                    context_window: model["maxInputTokens"].as_u64(),
                })
                .collect())
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
                    "Antigravity provider namespace mismatch".into(),
                ));
            }
            let project = self.discover_project(&context).await?;
            let headers = BTreeMap::from([(
                "user-agent".into(),
                "antigravity/ide/2.5.5 (os_type=linux; arch=x64; aidev_client; auth_method=oauth)"
                    .into(),
            )]);
            let codec = GoogleCodec::new(
                format!("{}/v1internal:streamGenerateContent?alt=sse", self.base_url),
                headers,
                project,
                &request.tools,
            );
            Ok(Box::new(
                HttpSession::start(self.client.clone(), Box::new(codec), request, context).await?,
            ) as Box<dyn ProviderSession>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    fn context() -> Context {
        Context {
            credentials: Credentials {
                access_token: "test-access".into(),
                account_id: "account-a".into(),
                metadata: json!({}),
            },
            cancellation: Default::default(),
        }
    }
    #[tokio::test]
    async fn project_discovery_onboards_once_and_discovers_real_catalog() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:loadCodeAssist"))
            .and(header("authorization", "Bearer test-access"))
            .and(body_json(json!({"metadata":{"ideType":"ANTIGRAVITY"}})))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"allowedTiers":[{"id":"source-default","isDefault":true}]}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1internal:onboardUser"))
            .and(wiremock::matchers::body_partial_json(
                json!({"tier_id":"source-default"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"done":true,"response":{"cloudaicompanionProject":{"id":"project-a"}}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST")).and(path("/v1internal:fetchAvailableModels")).and(body_json(json!({}))).respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":{"gemini-test":{"displayName":"Gemini Test","maxInputTokens":12345}}}))).expect(1).mount(&server).await;
        let provider = AntigravityProvider::with_base_url(Client::new(), server.uri()).unwrap();
        assert_eq!(
            provider.discover_project(&context()).await.unwrap(),
            "project-a"
        );
        let models = provider.models(context()).await.unwrap();
        assert_eq!(
            models,
            vec![Model {
                id: "gemini-test".into(),
                name: "Gemini Test".into(),
                context_window: Some(12345)
            }]
        );
        server.verify().await;
    }
    #[tokio::test]
    async fn supplied_project_avoids_control_plane_requests() {
        let server = MockServer::start().await;
        let provider = AntigravityProvider::with_base_url(Client::new(), server.uri()).unwrap();
        let mut ctx = context();
        ctx.credentials.metadata = json!({"project_id":"supplied-project"});
        assert_eq!(
            provider.discover_project(&ctx).await.unwrap(),
            "supplied-project"
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }
    #[test]
    fn project_extraction_accepts_observed_string_and_object_shapes() {
        assert_eq!(
            extract_project(&json!({"cloudaicompanionProject":"p"})),
            Some("p".into())
        );
        assert_eq!(
            extract_project(&json!({"project":{"id":"p"}})),
            Some("p".into())
        );
        assert_eq!(extract_project(&json!({"project":" "})), None);
    }
}
