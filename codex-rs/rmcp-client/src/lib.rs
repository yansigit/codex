mod auth_status;
mod bounded_stdio_transport;
mod elicitation_client_service;
mod ema_auth;
mod ema_auth_policy;
mod ema_claims;
mod ema_exchange;
mod ema_http_client;
mod ema_identity;
mod ema_resource;
mod enterprise_oauth_login;
mod event_notification_transport;
mod executor_process_transport;
mod http_client_adapter;
mod http_client_redirect;
mod http_headers;
mod in_process_transport;
mod local_stdio_transport;
mod logging_client_handler;
mod oauth;
mod oauth_callback;
mod oauth_client_credentials;
mod oauth_client_registration;
mod oauth_http_client;
mod oauth_refresh_mode;
mod perform_oauth_login;
mod program_resolver;
mod protocol_mode;
mod rmcp_client;
mod service_error;
mod startup_error;
mod stdio_server_launcher;
mod tool_input;
mod trace_context;
mod user_verification;
mod utils;
mod www_authenticate;

pub use auth_status::McpAuthState;
pub use auth_status::McpLoginRequirement;
pub use auth_status::OAuthDiscoveryTimeout;
pub use auth_status::StreamableHttpOAuthDiscovery;
pub use auth_status::determine_streamable_http_auth_status;
pub use auth_status::determine_streamable_http_auth_status_from_credentials;
pub use auth_status::discover_streamable_http_oauth;
pub use codex_protocol::protocol::McpAuthStatus;
pub use ema_auth::EmaAuthTokenExchangeRequest;
pub use ema_auth::exchange_ema_auth_token;
pub use ema_auth_policy::EmaAuthFailure;
pub use ema_auth_policy::EmaInvalidGrantSource;
pub use ema_auth_policy::validate_ema_auth_resource;
pub use ema_exchange::EmaAccessToken;
pub use ema_http_client::EmaAuthenticatedHttpClient;
pub use ema_http_client::EmaRequestAuthority;
pub use ema_http_client::EmaTokenSource;
pub use ema_identity::EmaIdpIdentity;
pub use ema_identity::EmaIdpIdentityRequest;
pub use ema_identity::resolve_ema_idp_identity;
pub use ema_identity::stored_ema_identity_is_usable;
pub use enterprise_oauth_login::EnterpriseOAuthCredentialGuard;
pub use enterprise_oauth_login::EnterpriseOAuthCredentials;
pub use enterprise_oauth_login::EnterpriseOAuthLoginHandle;
pub use enterprise_oauth_login::EnterpriseOAuthLoginRequest;
pub use enterprise_oauth_login::delete_enterprise_oauth_tokens;
pub use enterprise_oauth_login::perform_enterprise_oauth_login_return_url;
pub use event_notification_transport::EventNotificationReceiver;
pub use http_client_adapter::StreamableHttpRedirectMode;
pub use http_headers::with_http_headers_helper;
pub use in_process_transport::InProcessTransportFactory;
pub use oauth::EmaCredentialLease;
pub use oauth::EmaCredentialSnapshot;
pub use oauth::StoredOAuthCredentialSnapshot;
pub use oauth::StoredOAuthTokens;
pub use oauth::WrappedOAuthTokenResponse;
pub use oauth::delete_oauth_tokens;
pub use oauth::save_oauth_tokens;
pub use oauth::stored_oauth_credential_snapshot;
pub use oauth::stored_oauth_credentials;
pub use oauth_callback::McpOAuthCallbackMode;
pub use oauth_callback::resolve_mcp_oauth_callback_url;
pub use oauth_client_registration::McpOAuthClientRegistration;
pub use oauth_refresh_mode::McpOAuthRefreshMode;
pub use perform_oauth_login::OAuthProviderError;
pub use perform_oauth_login::OauthLoginHandle;
pub use perform_oauth_login::perform_oauth_login;
pub use perform_oauth_login::perform_oauth_login_return_url;
pub use perform_oauth_login::perform_oauth_login_silent;
pub use perform_oauth_login::perform_oauth_login_with_callback_input;
pub use protocol_mode::McpProtocolMode;
pub use rmcp::model::ElicitationAction;
pub use rmcp_client::CancellableEventStreamRequest;
pub use rmcp_client::Elicitation;
pub use rmcp_client::ElicitationResponse;
pub use rmcp_client::ListToolsWithConnectorIdResult;
pub use rmcp_client::RmcpClient;
pub use rmcp_client::SendElicitation;
pub use rmcp_client::StreamableHttpBearerToken;
pub use rmcp_client::ToolWithConnectorId;
pub use service_error::mcp_error;
pub use service_error::request_error_code;
pub use service_error::request_timeout_error;
pub use startup_error::is_authentication_required_error;
pub use stdio_server_launcher::ExecutorStdioServerLauncher;
pub use stdio_server_launcher::LocalStdioServerLauncher;
pub use stdio_server_launcher::StdioServerLauncher;
pub use user_verification::UserVerificationReason;

pub use perform_oauth_login::disconnect_oauth;

pub mod host_privacy {
    use oauth2::TokenResponse;
    use serde::Serialize;
    use serde_json::Value;
    use std::sync::OnceLock;
    use std::sync::RwLock;
    struct Secrets {
        values: Vec<String>,
        saturated: bool,
    }
    static HOST: OnceLock<RwLock<Secrets>> = OnceLock::new();
    pub fn enabled() -> bool {
        HOST.get().is_some()
    }
    pub fn load() -> std::io::Result<()> {
        let values: Vec<String> = match std::env::var_os("CODEX_HOST_SECRET_REDACTIONS") {
            None => Vec::new(),
            Some(path) => {
                let bytes = std::fs::read(path)?;
                if bytes.len() > 2 * 1024 * 1024 {
                    return Err(std::io::Error::other(
                        "host credential inventory exceeds limit",
                    ));
                }
                serde_json::from_slice(&bytes)
                    .map_err(|_| std::io::Error::other("invalid host credential inventory"))?
            }
        };
        if values.len() > 1024 || values.iter().any(|v| v.is_empty() || v.len() > 10000) {
            return Err(std::io::Error::other("invalid host credential inventory"));
        }
        HOST.set(RwLock::new(Secrets {
            values: Vec::new(),
            saturated: false,
        }))
        .map_err(|_| std::io::Error::other("host credential inventory already captured"))?;
        for value in values {
            register(&value);
        }
        Ok(())
    }
    pub fn register(value: &str) {
        let Some(host) = HOST.get() else {
            return;
        };
        if value.is_empty() {
            return;
        }
        let mut host = host
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if host.values.iter().any(|v| v == value) {
            return;
        }
        if host.values.len() >= 4096 || value.len() > 10000 {
            host.saturated = true;
            return;
        }
        host.values.push(value.to_owned());
    }
    pub fn register_tokens(tokens: &crate::StoredOAuthTokens) {
        register(tokens.token_response.0.access_token().secret());
        if let Some(refresh) = tokens.token_response.0.refresh_token() {
            register(refresh.secret());
        }
    }
    pub fn contains<T: Serialize>(value: &T) -> bool {
        let Some(host) = HOST.get() else {
            return false;
        };
        let host = host
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if host.saturated {
            return true;
        }
        if host.values.is_empty() {
            return false;
        }
        let Ok(value) = serde_json::to_value(value) else {
            return true;
        };
        fn visit(value: &Value, secrets: &[String]) -> bool {
            match value {
                Value::String(text) => secrets.iter().any(|s| text.contains(s)),
                Value::Array(items) => items.iter().any(|v| visit(v, secrets)),
                Value::Object(items) => items
                    .iter()
                    .any(|(k, v)| secrets.iter().any(|s| k.contains(s)) || visit(v, secrets)),
                _ => false,
            }
        }
        visit(&value, &host.values)
    }
}
