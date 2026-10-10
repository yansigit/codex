//! Adapter between compiled native providers and the existing model registry.
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use codex_login::{AuthManager, CodexAuth};
use codex_model_provider_info::{ModelProviderInfo, RemoteCompactionSupport};
use codex_models_manager::manager::{
    ModelsEndpointClient, ModelsEndpointFuture, ModelsEndpointResponse, OpenAiModelsManager,
    SharedModelsManager, StaticModelsManager,
};
use codex_protocol::error::CodexErr;
use codex_protocol::openai_models::ModelsResponse;
use codex_provider_api::{Context, Credentials, Provider, ProviderError, ProviderFuture};
use codex_provider_auth::{AuthStore, OAuthConfig};
use tokio_util::sync::CancellationToken;

use crate::{
    ModelProvider, ModelProviderFuture, ProviderAccountResult, ProviderAccountState,
    ProviderCapabilities,
};

#[derive(Clone)]
pub struct NativeModelProvider {
    info: ModelProviderInfo,
    home: Option<PathBuf>,
    backend: Arc<OnceLock<Arc<dyn Provider>>>,
}
impl std::fmt::Debug for NativeModelProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeModelProvider")
            .field("info", &self.info)
            .field("home", &self.home)
            .finish_non_exhaustive()
    }
}
impl NativeModelProvider {
    pub fn new(info: ModelProviderInfo, home: Option<PathBuf>) -> Self {
        Self {
            info,
            home,
            backend: Arc::new(OnceLock::new()),
        }
    }
    fn id(&self) -> &'static str {
        self.info
            .wire_api
            .native_provider_id()
            .unwrap_or("unknown-native-provider")
    }
    fn auth_store(&self) -> codex_provider_api::Result<AuthStore> {
        self.home.as_deref().map(AuthStore::new).ok_or_else(|| {
            ProviderError::Authentication("native provider requires a configured Codex home".into())
        })
    }
    fn manager(&self, catalog: Option<ModelsResponse>) -> SharedModelsManager {
        if let Some(catalog) = catalog {
            return Arc::new(StaticModelsManager::new(None, catalog));
        }
        Arc::new(
            OpenAiModelsManager::new_without_cache(Arc::new(self.clone()), None)
                .with_provider_catalog(),
        )
    }
    fn environment_token(&self) -> Option<String> {
        self.info
            .experimental_bearer_token
            .as_ref()
            .map(|token| token.as_str().to_owned())
            .or_else(|| {
                self.info
                    .env_key
                    .as_ref()
                    .and_then(|key| std::env::var(key).ok())
            })
            .filter(|token| !token.trim().is_empty())
    }
    /// Whether authentication is supplied by configuration rather than the account store.
    pub fn has_configured_token(&self) -> bool {
        self.environment_token().is_some()
    }
    fn scope_account(&self, account: String) -> String {
        match &self.info.base_url {
            Some(url) => format!("{account}@{}", codex_provider_auth::digest(url.as_bytes())),
            None => account,
        }
    }
    fn scope_credentials(&self, mut credentials: Credentials) -> Credentials {
        credentials.account_id = self.scope_account(credentials.account_id);
        credentials
    }
    fn auxiliary_model(&self) -> &'static str {
        match self.id() {
            "cursor" => "gpt-5.6-luna-none",
            "opencode-go" => "glm-5.3",
            "claude" => "claude-haiku-5-5",
            "antigravity" => "gemini-3.5-flash-lite",
            _ => unreachable!("native provider ID"),
        }
    }
}

/// Compiled registrations are filled by the provider feature modules.
pub fn provider_for(
    info: &ModelProviderInfo,
    state_home: Option<&std::path::Path>,
) -> codex_provider_api::Result<Arc<dyn Provider>> {
    #[cfg(not(feature = "provider-antigravity"))]
    let _ = state_home;
    let id = info
        .wire_api
        .native_provider_id()
        .ok_or_else(|| ProviderError::Unsupported("not a native provider".into()))?;
    #[cfg(any(
        feature = "provider-opencode",
        feature = "provider-claude",
        feature = "provider-antigravity"
    ))]
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| ProviderError::Transport("failed to create native HTTP client".into()))?;
    #[cfg(any(
        feature = "provider-opencode",
        feature = "provider-claude",
        feature = "provider-antigravity"
    ))]
    macro_rules! register {
        ($provider:ty) => {{
            let provider = match &info.base_url {
                Some(url) => <$provider>::with_base_url(client, url.clone())?,
                None => <$provider>::new(client),
            };
            return Ok(Arc::new(provider));
        }};
    }
    match id {
        #[cfg(feature = "provider-cursor")]
        "cursor" => {
            return Ok(Arc::new(codex_provider_cursor::CursorProvider::new(
                codex_provider_cursor::Options {
                    base_url: info
                        .base_url
                        .clone()
                        .unwrap_or_else(|| "https://api2.cursor.sh".into()),
                    ..Default::default()
                },
            )?));
        }
        #[cfg(feature = "provider-opencode")]
        "opencode-go" => register!(codex_provider_opencode::OpenCodeProvider),
        #[cfg(feature = "provider-claude")]
        "claude" => register!(codex_provider_claude::ClaudeProvider),
        #[cfg(feature = "provider-antigravity")]
        "antigravity" => {
            let provider = match &info.base_url {
                Some(url) => codex_provider_antigravity::AntigravityProvider::with_base_url(
                    client,
                    url.clone(),
                )?,
                None => codex_provider_antigravity::AntigravityProvider::new(client),
            };
            let home = state_home.ok_or_else(|| {
                ProviderError::Checkpoint(
                    "Antigravity onboarding requires local state storage".into(),
                )
            })?;
            return Ok(Arc::new(provider.with_state_home(home.to_path_buf())));
        }
        _ => {}
    }
    Err(ProviderError::Unsupported(format!(
        "provider {id} was not compiled into this binary"
    )))
}

pub fn oauth_config(id: &str) -> Option<OAuthConfig> {
    #[cfg(any(feature = "provider-claude", feature = "provider-antigravity"))]
    use std::collections::BTreeMap;
    match id {
        #[cfg(feature = "provider-claude")]
        "claude" => Some(OAuthConfig {
            authorization_url: codex_provider_claude::AUTHORIZE_URL.into(),
            token_url: codex_provider_claude::TOKEN_URL.into(),
            client_id: codex_provider_claude::CLIENT_ID.into(),
            redirect_uri: codex_provider_claude::REDIRECT_URI.into(),
            scopes: codex_provider_claude::SCOPES
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            authorization_params: BTreeMap::from([("code".into(), "true".into())]),
            token_params: BTreeMap::new(),
            token_json: true,
            exchange_state: true,
        }),
        #[cfg(feature = "provider-antigravity")]
        "antigravity" => Some(OAuthConfig {
            authorization_url: codex_provider_antigravity::AUTHORIZE_URL.into(),
            token_url: codex_provider_antigravity::TOKEN_URL.into(),
            client_id: codex_provider_antigravity::CLIENT_ID.into(),
            redirect_uri: codex_provider_antigravity::REDIRECT_URI.into(),
            scopes: codex_provider_antigravity::SCOPES
                .iter()
                .map(|value| (*value).into())
                .collect(),
            authorization_params: BTreeMap::from([
                ("access_type".into(), "offline".into()),
                ("prompt".into(), "consent".into()),
            ]),
            // Public desktop OAuth registration, as published by the pinned Antigravity auth reference.
            token_params: BTreeMap::from([(
                "client_secret".into(),
                codex_provider_antigravity::CLIENT_SECRET.into(),
            )]),
            token_json: false,
            exchange_state: false,
        }),
        _ => None,
    }
}

pub enum NativeLogin {
    OAuth(Box<codex_provider_auth::LoopbackLogin>),
    #[cfg(feature = "provider-cursor")]
    Cursor(codex_provider_cursor::auth::LoginChallenge),
}
impl NativeLogin {
    pub async fn begin(id: &str) -> codex_provider_api::Result<Self> {
        #[cfg(feature = "provider-cursor")]
        if id == "cursor" {
            return Ok(Self::Cursor(
                codex_provider_cursor::auth::LoginChallenge::new(),
            ));
        }
        let config = oauth_config(id)
            .ok_or_else(|| ProviderError::Unsupported(format!("{id} requires an API key")))?;
        Ok(Self::OAuth(Box::new(
            codex_provider_auth::LoopbackLogin::begin(config).await?,
        )))
    }
    pub fn url(&self) -> &str {
        match self {
            Self::OAuth(login) => &login.auth_url,
            #[cfg(feature = "provider-cursor")]
            Self::Cursor(login) => &login.url,
        }
    }
    pub async fn finish(
        self,
        cancellation: &CancellationToken,
    ) -> codex_provider_api::Result<codex_provider_auth::TokenRecord> {
        match self {
            Self::OAuth(login) => login.finish(cancellation).await,
            #[cfg(feature = "provider-cursor")]
            Self::Cursor(challenge) => {
                let auth = codex_provider_cursor::auth::AuthClient::new("https://api2.cursor.sh")?
                    .poll_auth(challenge, cancellation.clone())
                    .await?;
                Ok(codex_provider_auth::TokenRecord {
                    expires_at: codex_provider_cursor::auth::token_expires_at(&auth.access_token),
                    access_token: auth.access_token,
                    refresh_token: auth.refresh_token,
                    metadata: serde_json::Value::Null,
                    revoked: false,
                })
            }
        }
    }
}

impl ModelProvider for NativeModelProvider {
    fn native_state_home(&self) -> Option<PathBuf> {
        self.home.clone()
    }
    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }
    fn native_provider(&self) -> codex_provider_api::Result<Arc<dyn Provider>> {
        if let Some(provider) = self.backend.get() {
            return Ok(Arc::clone(provider));
        }
        let provider = provider_for(&self.info, self.home.as_deref())?;
        let _ = self.backend.set(Arc::clone(&provider));
        Ok(self.backend.get().cloned().unwrap_or(provider))
    }
    fn native_credentials(
        &self,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Credentials> {
        Box::pin(async move {
            let token = self.environment_token();
            if let Some(access_token) = token {
                return Ok(self.scope_credentials(Credentials {
                    account_id: format!(
                        "token-{}",
                        codex_provider_auth::digest(access_token.as_bytes())
                    ),
                    access_token,
                    metadata: serde_json::Value::Null,
                }));
            }
            #[cfg(feature = "provider-cursor")]
            if self.id() == "cursor" {
                return self
                    .auth_store()?
                    .credentials_with_refresh(
                        self.id(),
                        &cancellation,
                        |record, refresh_cancel| async move {
                            let refresh = record.refresh_token.ok_or_else(|| {
                                ProviderError::Authentication("Cursor refresh token missing".into())
                            })?;
                            let auth = codex_provider_cursor::auth::AuthClient::new(
                                "https://api2.cursor.sh",
                            )?
                            .refresh_auth(refresh, refresh_cancel)
                            .await?;
                            Ok(codex_provider_auth::TokenRecord {
                                expires_at: codex_provider_cursor::auth::token_expires_at(
                                    &auth.access_token,
                                ),
                                access_token: auth.access_token,
                                refresh_token: auth.refresh_token,
                                metadata: record.metadata,
                                revoked: false,
                            })
                        },
                    )
                    .await
                    .map(|credentials| self.scope_credentials(credentials));
            }
            self.auth_store()?
                .credentials(self.id(), oauth_config(self.id()).as_ref(), &cancellation)
                .await
                .map(|credentials| self.scope_credentials(credentials))
        })
    }
    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        None
    }
    fn auth(&self) -> ModelProviderFuture<'_, Option<CodexAuth>> {
        Box::pin(async { None })
    }
    fn account_state(&self) -> ProviderAccountResult {
        if self.environment_token().is_some() {
            return Ok(ProviderAccountState {
                account: Some(codex_protocol::account::ProviderAccount::Native {
                    provider: self.id().into(),
                    account_id: "configured token".into(),
                }),
                requires_openai_auth: false,
            });
        }
        let status = self
            .auth_store()
            .and_then(|store| store.status(self.id()))
            .map_err(|_| crate::ProviderAccountError::NativeStateUnavailable)?;
        Ok(ProviderAccountState {
            account: status.filter(|status| !status.revoked).map(|status| {
                codex_protocol::account::ProviderAccount::Native {
                    provider: status.provider,
                    account_id: status.account,
                }
            }),
            requires_openai_auth: false,
        })
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            image_generation: false,
            web_search: false,
            external_web_access: false,
            remote_compaction: RemoteCompactionSupport::Unsupported,
        }
    }
    fn approval_review_preferred_model(&self) -> &'static str {
        self.auxiliary_model()
    }
    fn memory_extraction_preferred_model(&self) -> &'static str {
        self.auxiliary_model()
    }
    fn memory_consolidation_preferred_model(&self) -> &'static str {
        self.auxiliary_model()
    }
    fn models_manager(
        &self,
        _home: PathBuf,
        catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        self.manager(catalog)
    }
    fn models_manager_without_cache(&self, catalog: Option<ModelsResponse>) -> SharedModelsManager {
        self.manager(catalog)
    }
}

impl ModelsEndpointClient for NativeModelProvider {
    fn identity(&self) -> Option<String> {
        let account = match self.environment_token() {
            Some(token) => format!("token-{}", codex_provider_auth::digest(token.as_bytes())),
            None => self.auth_store().ok()?.identity(self.id()).ok()??,
        };
        let account = self.scope_account(account);
        Some(codex_provider_auth::digest(
            format!("{}:{account}:{:?}", self.id(), self.info.base_url).as_bytes(),
        ))
    }
    fn has_command_auth(&self) -> bool {
        true
    }
    fn uses_codex_backend(&self) -> ModelsEndpointFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn list_models<'a>(
        &'a self,
        _: &'a str,
        _: codex_http_client::HttpClientFactory,
    ) -> ModelsEndpointFuture<'a, codex_protocol::error::Result<ModelsEndpointResponse>> {
        Box::pin(async move {
            let cancellation = CancellationToken::new();
            let credentials = self
                .native_credentials(cancellation.clone())
                .await
                .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?;
            let identity = codex_provider_auth::digest(
                format!(
                    "{}:{}:{:?}",
                    self.id(),
                    credentials.account_id,
                    self.info.base_url
                )
                .as_bytes(),
            );
            let models = self
                .native_provider()
                .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?
                .models(Context {
                    credentials,
                    cancellation,
                })
                .await
                .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?;
            let models = models
                .into_iter()
                .map(|model| {
                    let mut info =
                        codex_models_manager::model_info::model_info_from_slug(&model.id);
                    info.display_name = model.name;
                    info.context_window =
                        model.context_window.and_then(|value| value.try_into().ok());
                    info.max_context_window = info.context_window;
                    if let Some(supports_images) = model.supports_images {
                        info.input_modalities =
                            vec![codex_protocol::openai_models::InputModality::Text];
                        if supports_images {
                            info.input_modalities
                                .push(codex_protocol::openai_models::InputModality::Image);
                        }
                    }
                    info.supported_in_api = true;
                    info.visibility = codex_protocol::openai_models::ModelVisibility::List;
                    info
                })
                .collect();
            Ok(ModelsEndpointResponse {
                models,
                etag: None,
                identity,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_provider_api::{Checkpoint, Namespace};
    use codex_provider_auth::SessionStore;
    use serde_json::json;

    #[tokio::test]
    async fn changing_native_endpoint_cannot_restore_another_endpoints_checkpoint() {
        let home = tempfile::tempdir().unwrap();
        let mut info = codex_model_provider_info::built_in_model_providers(None)
            .remove("opencode-go")
            .unwrap();
        info.experimental_bearer_token = Some("fixture-token".into());
        info.base_url = Some("https://first.example.test".into());
        let first = NativeModelProvider::new(info.clone(), Some(home.path().into()));
        info.base_url = Some("https://second.example.test".into());
        let second = NativeModelProvider::new(info, Some(home.path().into()));
        let cancellation = CancellationToken::new();
        let credentials = first
            .native_credentials(cancellation.clone())
            .await
            .unwrap();
        let namespace = Namespace {
            provider: "opencode-go".into(),
            account: credentials.account_id,
            model: "fixture".into(),
            thread: "thread".into(),
        };
        SessionStore::new(home.path(), namespace.clone())
            .save(
                &Checkpoint {
                    version: 1,
                    namespace: namespace.clone(),
                    context_hash: "history".into(),
                    state: json!({"private_continuation":"first endpoint"}),
                },
                &cancellation,
            )
            .await
            .unwrap();
        let credentials = second.native_credentials(cancellation).await.unwrap();
        assert_ne!(credentials.account_id, namespace.account);
        assert_ne!(first.identity(), second.identity());
        let other = Namespace {
            account: credentials.account_id,
            ..namespace
        };
        assert!(
            SessionStore::new(home.path(), other)
                .load()
                .unwrap()
                .is_none()
        );
    }
}
