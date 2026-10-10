//! Codex-facing native backend. Provider transport and authentication live in the registry.
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::{
    ModelsEndpointClient, ModelsEndpointFuture, ModelsEndpointResponse, OpenAiModelsManager,
    SharedModelsManager, StaticModelsManager,
};
use codex_protocol::error::CodexErr;
use codex_protocol::openai_models::ModelsResponse;
use codex_provider_api::{Context, Credentials, Provider, ProviderError, ProviderFuture};
use codex_provider_auth::{AccountInfo, AuthStore, TokenRecord};
pub use codex_provider_registry::NativeLogin;
use codex_provider_registry::{Descriptor, TransportOptions, registry};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionCapabilities {
    pub automatic_inference_retry: bool,
    pub terminal_completion: bool,
    pub structured_host_completion: bool,
}
#[derive(Clone)]
pub struct NativeBackend {
    info: ModelProviderInfo,
    descriptor: Option<&'static Descriptor>,
    home: Option<PathBuf>,
    transport: Arc<OnceLock<Arc<dyn Provider>>>,
}
impl std::fmt::Debug for NativeBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeBackend")
            .field("info", &self.info)
            .field("home", &self.home)
            .finish_non_exhaustive()
    }
}
impl NativeBackend {
    pub fn new(info: ModelProviderInfo, home: Option<PathBuf>) -> Self {
        Self {
            descriptor: info.native_provider_id().and_then(|id| registry().get(id)),
            info,
            home,
            transport: Arc::new(OnceLock::new()),
        }
    }
    pub fn id(&self) -> &str {
        self.info
            .native_provider_id()
            .unwrap_or("unknown-native-provider")
    }
    pub fn info(&self) -> &ModelProviderInfo {
        &self.info
    }
    pub fn state_home(&self) -> Option<PathBuf> {
        self.home.clone()
    }
    pub fn descriptor(&self) -> codex_provider_api::Result<&'static Descriptor> {
        let descriptor = self.descriptor.ok_or_else(|| {
            ProviderError::Unsupported(format!("unknown native provider: {}", self.id()))
        })?;
        if descriptor.id != self.id() {
            return Err(ProviderError::Protocol(
                "native backend descriptor conflicts with provider configuration".into(),
            ));
        }
        Ok(descriptor)
    }
    pub fn session_capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            automatic_inference_retry: false,
            terminal_completion: true,
            structured_host_completion: true,
        }
    }
    pub fn with_provider(
        info: ModelProviderInfo,
        home: Option<PathBuf>,
        provider: Arc<dyn Provider>,
    ) -> Self {
        let backend = Self::new(info, home);
        let _ = backend.transport.set(provider);
        backend
    }
    pub fn with_descriptor(
        info: ModelProviderInfo,
        home: Option<PathBuf>,
        descriptor: &'static Descriptor,
    ) -> Self {
        Self {
            info,
            home,
            descriptor: Some(descriptor),
            transport: Arc::new(OnceLock::new()),
        }
    }
    fn auth_store(&self) -> codex_provider_api::Result<AuthStore> {
        self.home.as_deref().map(AuthStore::new).ok_or_else(|| {
            ProviderError::Authentication("native provider requires a configured Codex home".into())
        })
    }
    pub fn models_manager(&self, catalog: Option<ModelsResponse>) -> SharedModelsManager {
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
}

impl NativeBackend {
    pub fn provider(&self) -> codex_provider_api::Result<Arc<dyn Provider>> {
        let descriptor = self.descriptor()?;
        if let Some(provider) = self.transport.get() {
            return Ok(Arc::clone(provider));
        }
        let provider = descriptor.provider(TransportOptions {
            base_url: self.info.base_url.as_deref(),
            state_home: self.home.as_deref(),
        })?;
        let _ = self.transport.set(Arc::clone(&provider));
        Ok(self.transport.get().cloned().unwrap_or(provider))
    }
    pub fn credentials(&self, cancellation: CancellationToken) -> ProviderFuture<'_, Credentials> {
        Box::pin(async move {
            let descriptor = self.descriptor()?;
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
            descriptor
                .credentials(&self.auth_store()?, &cancellation)
                .await
                .map(|credentials| self.scope_credentials(credentials))
        })
    }
    fn ensure_stored_auth(&self) -> codex_provider_api::Result<()> {
        self.descriptor()?;
        if self.has_configured_token() {
            return Err(ProviderError::Authentication("remove the configured environment or bearer token override before changing stored login".into()));
        }
        Ok(())
    }
    pub async fn begin_login(&self) -> codex_provider_api::Result<NativeLogin> {
        self.ensure_stored_auth()?;
        NativeLogin::begin(self.descriptor()?).await
    }
    pub async fn commit_login(
        &self,
        account: &str,
        record: TokenRecord,
        cancellation: &CancellationToken,
    ) -> codex_provider_api::Result<()> {
        self.ensure_stored_auth()?;
        self.auth_store()?
            .login(self.id(), account, record, cancellation)
            .await
    }
    pub async fn login_token(
        &self,
        account: &str,
        token: String,
        cancellation: &CancellationToken,
    ) -> codex_provider_api::Result<()> {
        self.commit_login(
            account,
            TokenRecord {
                access_token: token,
                refresh_token: None,
                expires_at: None,
                metadata: serde_json::Value::Null,
                revoked: false,
            },
            cancellation,
        )
        .await
    }
    pub async fn logout(&self, cancellation: &CancellationToken) -> codex_provider_api::Result<()> {
        self.ensure_stored_auth()?;
        self.auth_store()?.logout(self.id(), cancellation).await
    }
    pub fn status(&self) -> codex_provider_api::Result<Option<AccountInfo>> {
        self.descriptor()?;
        if self.has_configured_token() {
            return Ok(Some(AccountInfo {
                provider: self.id().into(),
                account: "configured token".into(),
                expires_at: None,
                revoked: false,
            }));
        }
        self.auth_store()?.status(self.id())
    }
    pub fn account(
        &self,
    ) -> codex_provider_api::Result<Option<codex_protocol::account::ProviderAccount>> {
        self.descriptor()?;
        if self.environment_token().is_some() {
            return Ok(Some(codex_protocol::account::ProviderAccount::Native {
                provider: self.id().into(),
                account_id: "configured token".into(),
            }));
        }
        let status = self.status()?;
        Ok(status.filter(|status| !status.revoked).map(|status| {
            codex_protocol::account::ProviderAccount::Native {
                provider: status.provider,
                account_id: status.account,
            }
        }))
    }
}
impl ModelsEndpointClient for NativeBackend {
    fn identity(&self) -> Option<String> {
        self.descriptor().ok()?;
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
                .credentials(cancellation.clone())
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
                .provider()
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
        let first = NativeBackend::new(info.clone(), Some(home.path().into()));
        info.base_url = Some("https://second.example.test".into());
        let second = NativeBackend::new(info, Some(home.path().into()));
        let cancellation = CancellationToken::new();
        let credentials = first.credentials(cancellation.clone()).await.unwrap();
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
        let credentials = second.credentials(cancellation).await.unwrap();
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
    struct FifthProvider;
    impl Provider for FifthProvider {
        fn id(&self) -> &'static str {
            "fifth-provider"
        }
        fn models(&self, context: Context) -> ProviderFuture<'_, Vec<codex_provider_api::Model>> {
            assert_eq!(context.credentials.access_token, "fixture-token");
            Box::pin(async {
                Ok(vec![codex_provider_api::Model {
                    id: "fifth-model".into(),
                    name: "Fifth model".into(),
                    context_window: Some(120_000),
                    supports_images: Some(true),
                }])
            })
        }
        fn start(
            &self,
            _request: codex_provider_api::Request,
            _context: Context,
        ) -> ProviderFuture<'_, Box<dyn codex_provider_api::ProviderSession>> {
            Box::pin(async {
                Err(ProviderError::Unsupported(
                    "catalog fixture does not infer".into(),
                ))
            })
        }
    }
    fn fifth_factory(
        options: TransportOptions<'_>,
    ) -> codex_provider_api::Result<Arc<dyn Provider>> {
        assert_eq!(options.base_url, Some("https://fifth.example.test"));
        assert!(options.state_home.is_some());
        Ok(Arc::new(FifthProvider))
    }
    static FIFTH: Descriptor = Descriptor {
        id: "fifth-provider",
        display_name: "Fifth provider",
        env_key: None,
        auxiliary_model: "fifth-model",
        legacy_wire_aliases: &[],
        factory: Some(fifth_factory),
        auth_driver: Some(&codex_provider_registry::AuthDriver::ApiKey),
        ..codex_provider_registry::OPENCODE
    };

    #[tokio::test]
    async fn registered_fifth_provider_uses_factory_catalog_credentials_and_account() {
        let mut registry = registry().clone();
        registry.register(&FIFTH).unwrap();
        let home = tempfile::tempdir().unwrap();
        let info = ModelProviderInfo {
            name: "Fifth".into(),
            wire_api: codex_model_provider_info::WireApi::Native,
            native_provider: Some("fifth-provider".into()),
            base_url: Some("https://fifth.example.test".into()),
            ..Default::default()
        };
        info.validate_with_registry(&registry).unwrap();
        let descriptor = registry.get(info.native_provider_id().unwrap()).unwrap();
        let backend = NativeBackend::with_descriptor(info, Some(home.path().into()), descriptor);
        let cancellation = CancellationToken::new();
        backend
            .login_token("account-five", "fixture-token".into(), &cancellation)
            .await
            .unwrap();
        assert_eq!(backend.id(), "fifth-provider");
        assert_eq!(backend.descriptor().unwrap().auxiliary_model, "fifth-model");
        assert!(Arc::ptr_eq(
            &backend.provider().unwrap(),
            &backend.provider().unwrap()
        ));
        assert_eq!(
            backend.account().unwrap(),
            Some(codex_protocol::account::ProviderAccount::Native {
                provider: "fifth-provider".into(),
                account_id: "account-five".into()
            })
        );
        let factory = codex_http_client::HttpClientFactory::new(
            codex_http_client::OutboundProxyPolicy::ReqwestDefault,
        );
        let response = backend.list_models("fixture", factory).await.unwrap();
        assert_eq!(response.models[0].slug, "fifth-model");
        assert_eq!(response.models[0].display_name, "Fifth model");
        assert_eq!(response.models[0].context_window, Some(120_000));
        assert!(
            response.models[0]
                .input_modalities
                .contains(&codex_protocol::openai_models::InputModality::Image)
        );
        assert_eq!(
            backend.identity().as_deref(),
            Some(response.identity.as_str())
        );
        backend.logout(&cancellation).await.unwrap();
        assert_eq!(backend.account().unwrap(), None);
    }

    #[tokio::test]
    async fn unknown_native_provider_operations_return_errors_without_panicking() {
        let home = tempfile::tempdir().unwrap();
        let info = ModelProviderInfo {
            wire_api: codex_model_provider_info::WireApi::Native,
            native_provider: Some("unknown".into()),
            experimental_bearer_token: Some("configured-token".into()),
            ..Default::default()
        };
        let backend = NativeBackend::new(info, Some(home.path().into()));
        let cancellation = CancellationToken::new();
        assert!(backend.descriptor().is_err());
        assert!(backend.provider().is_err());
        assert!(backend.status().is_err());
        assert!(backend.account().is_err());
        assert!(backend.credentials(cancellation.clone()).await.is_err());
        assert!(backend.begin_login().await.is_err());
        assert!(
            backend
                .login_token("account", "token".into(), &cancellation)
                .await
                .is_err()
        );
        assert!(backend.logout(&cancellation).await.is_err());
    }
}
