use std::io::{IsTerminal, Read};

use clap::{Args, Subcommand};
use codex_model_provider::{ModelProvider, native::NativeModelProvider};
use codex_provider_auth::{AuthStore, TokenRecord};
use tokio_util::sync::CancellationToken;

const PROVIDERS: [&str; 4] = ["cursor", "opencode-go", "claude", "antigravity"];

#[derive(Debug, Args)]
pub(crate) struct ProviderCommand {
    #[command(subcommand)]
    action: ProviderAction,
}

#[derive(Debug, Subcommand)]
enum ProviderAction {
    /// Log in to one native provider. Tokens are read from stdin, never arguments.
    Login {
        #[arg(value_parser = PROVIDERS)]
        provider: String,
        #[arg(long, default_value = "default")]
        account: String,
        #[arg(long)]
        with_token: bool,
    },
    /// Remove the selected account for one provider.
    Logout {
        #[arg(value_parser = PROVIDERS)]
        provider: String,
    },
    /// Show selected native accounts without exposing credentials.
    Status {
        #[arg(value_parser = PROVIDERS)]
        provider: Option<String>,
    },
    /// Discover models using the selected provider account.
    Models {
        #[arg(value_parser = PROVIDERS)]
        provider: String,
    },
}

impl ProviderCommand {
    pub(crate) async fn run(
        self,
        overrides: codex_utils_cli::CliConfigOverrides,
    ) -> anyhow::Result<()> {
        let config = codex_core::config::Config::load_with_cli_overrides(
            overrides.parse_overrides().map_err(anyhow::Error::msg)?,
        )
        .await?;
        let home = config.codex_home;
        let provider_for = |id: &str| -> anyhow::Result<NativeModelProvider> {
            let info = if config.model_provider.wire_api.native_provider_id() == Some(id) {
                config.model_provider.clone()
            } else {
                codex_model_provider_info::built_in_model_providers(None)
                    .remove(id)
                    .ok_or_else(|| anyhow::anyhow!("unknown provider"))?
            };
            Ok(NativeModelProvider::new(info, Some(home.to_path_buf())))
        };
        let store = AuthStore::new(&home);
        let cancellation = CancellationToken::new();
        let signal = cancellation.clone();
        let listener = tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            signal.cancel();
        });
        let result: anyhow::Result<()> = async {
            match self.action {
                ProviderAction::Status { provider } => {
                    for id in provider
                        .as_deref()
                        .map(|id| vec![id])
                        .unwrap_or_else(|| PROVIDERS.to_vec())
                    {
                        if provider_for(id)?.has_configured_token() {
                            println!("{id}: configured token (environment or configuration)");
                            continue;
                        }
                        match store.status(id)? {
                            Some(status) => println!(
                                "{id}: {}{}",
                                status.account,
                                if status.revoked {
                                    " (login required)"
                                } else {
                                    ""
                                }
                            ),
                            None => println!("{id}: not logged in"),
                        }
                    }
                }
                ProviderAction::Logout { provider } => {
                    anyhow::ensure!(!provider_for(&provider)?.has_configured_token(), "remove the configured environment or bearer token override before logging out");
                    store.logout(&provider, &cancellation).await?;
                    println!("Logged out of {provider}");
                }
                ProviderAction::Login {
                    provider,
                    account,
                    with_token,
                } => {
                    anyhow::ensure!(!account.trim().is_empty(), "account name must not be empty");
                    anyhow::ensure!(!provider_for(&provider)?.has_configured_token(), "remove the configured environment or bearer token override before logging in");
                    let record = if with_token {
                        anyhow::ensure!(
                            !std::io::stdin().is_terminal(),
                            "pipe the token into stdin when using --with-token"
                        );
                        let mut token = String::new();
                        std::io::stdin()
                            .take(1024 * 1024 + 1)
                            .read_to_string(&mut token)?;
                        anyhow::ensure!(token.len() <= 1024 * 1024, "token exceeds 1 MiB");
                        let token = token.trim().to_owned();
                        anyhow::ensure!(!token.is_empty(), "empty token");
                        TokenRecord {
                            access_token: token,
                            refresh_token: None,
                            expires_at: None,
                            metadata: serde_json::Value::Null,
                            revoked: false,
                        }
                    } else {
                        let login =
                            codex_model_provider::native::NativeLogin::begin(&provider).await?;
                        println!("Open this URL to log in to {provider}:\n{}", login.url());
                        login.finish(&cancellation).await?
                    };
                    store
                        .login(&provider, &account, record, &cancellation)
                        .await?;
                    println!("Logged in to {provider} as {account}");
                }
                ProviderAction::Models { provider } => {
                    let provider = provider_for(&provider)?;
                    let credentials = provider.native_credentials(cancellation.clone()).await?;
                    let models = provider
                        .native_provider()?
                        .models(codex_provider_api::Context {
                            credentials,
                            cancellation,
                        })
                        .await?;
                    for model in models {
                        println!("{}\t{}", model.id, model.name);
                    }
                }
            }
            Ok(())
        }
        .await;
        listener.abort();
        result
    }
}
