use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_provider_auth::AuthStore;
use codex_provider_auth::TokenRecord;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const READ_TIMEOUT: Duration = Duration::from_secs(30);
const API_KEY: &str = "dummyfixturetoken";

fn write_claude_config(codex_home: &std::path::Path) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        r#"
model = "claude-haiku-5-5"
model_provider = "claude_native_fixture"
approval_policy = "never"
sandbox_mode = "danger-full-access"

[model_providers.claude_native_fixture]
name = "Claude native provider test"
wire_api = "claude"
env_key = "ANTHROPIC_API_KEY"
"#,
    )?;
    Ok(())
}

async fn login_claude_with_token(server: &mut TestAppServer) -> Result<()> {
    let request_id = server
        .send_login_account_request(json!({
            "type": "native",
            "provider": "claude",
            "accountId": "default",
            "apiKey": API_KEY,
        }))
        .await?;
    let response: Value = timeout(READ_TIMEOUT, server.read_response(request_id)).await??;
    assert_eq!(response["type"], "native");
    assert_eq!(response["provider"], "claude");
    assert!(response["loginId"].is_null());

    let completed = server
        .read_stream_until_notification_message("account/login/completed")
        .await?;
    let params = completed.params.expect("login completion params");
    assert_eq!(params["success"], true);
    assert!(params["loginId"].is_null());

    let updated = server
        .read_stream_until_notification_message("account/updated")
        .await?;
    let params = updated.params.expect("account update params");
    assert!(params["authMode"].is_null());
    Ok(())
}

fn seed_token(access_token: &str) -> TokenRecord {
    TokenRecord {
        access_token: access_token.into(),
        refresh_token: None,
        expires_at: None,
        metadata: json!({}),
        revoked: false,
    }
}

#[tokio::test]
async fn native_token_login_read_and_logout_are_provider_scoped() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_claude_config(codex_home.path())?;

    let auth_store = AuthStore::new(codex_home.path());
    auth_store
        .login(
            "opencode-go",
            "default",
            seed_token("go-dummyfixturetoken"),
            &CancellationToken::new(),
        )
        .await?;

    let mut server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("ANTHROPIC_API_KEY", None), ("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;

    login_claude_with_token(&mut server).await?;

    let request_id = server
        .send_get_account_request(codex_app_server_protocol::GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let account: Value = timeout(READ_TIMEOUT, server.read_response(request_id)).await??;
    assert_eq!(account["account"]["type"], "native");
    assert_eq!(account["account"]["provider"], "claude");
    assert_eq!(account["account"]["accountId"], "default");

    let request_id = server.send_logout_account_request().await?;
    let response: Value = timeout(READ_TIMEOUT, server.read_response(request_id)).await??;
    assert_eq!(response, json!({}));
    let updated = server
        .read_stream_until_notification_message("account/updated")
        .await?;
    assert!(updated.params.expect("account update params")["authMode"].is_null());

    assert_eq!(auth_store.status("claude")?, None);
    assert_eq!(
        auth_store
            .status("opencode-go")?
            .map(|status| status.account),
        Some("default".into())
    );
    Ok(())
}

#[tokio::test]
async fn environment_token_override_rejects_logout_without_deleting_stored_account() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_claude_config(codex_home.path())?;

    let auth_store = AuthStore::new(codex_home.path());
    auth_store
        .login(
            "claude",
            "default",
            seed_token("stored-dummyfixturetoken"),
            &CancellationToken::new(),
        )
        .await?;

    let mut server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("ANTHROPIC_API_KEY", Some(API_KEY)),
            ("OPENAI_API_KEY", None),
        ])
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;

    let request_id = server.send_logout_account_request().await?;
    let error = timeout(
        READ_TIMEOUT,
        server.read_stream_until_error_message(codex_app_server_protocol::RequestId::Integer(
            request_id,
        )),
    )
    .await??;
    assert!(
        error
            .error
            .message
            .contains("configured by an environment variable")
    );
    assert_eq!(
        auth_store.status("claude")?.map(|status| status.account),
        Some("default".into())
    );
    Ok(())
}

#[tokio::test]
async fn native_provider_selection_survives_fork_and_cold_resume() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        "model = \"gpt-5.6-luna\"\n",
    )?;
    AuthStore::new(codex_home.path())
        .login(
            "opencode-go",
            "default",
            seed_token("go-dummyfixturetoken"),
            &CancellationToken::new(),
        )
        .await?;

    let mut server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;

    let started: ThreadStartResponse = timeout(
        READ_TIMEOUT,
        server.start_thread(ThreadStartParams {
            model: Some("glm-5.3".into()),
            model_provider: Some("opencode-go".into()),
            ..Default::default()
        }),
    )
    .await??;
    assert_eq!(started.model_provider, "opencode-go");
    assert_eq!(started.model, "glm-5.3");

    let fork_id = server
        .send_thread_fork_request(ThreadForkParams {
            thread_id: started.thread.id.clone(),
            ..Default::default()
        })
        .await?;
    let forked: ThreadForkResponse = timeout(READ_TIMEOUT, server.read_response(fork_id)).await??;
    assert_eq!(forked.model_provider, "opencode-go");
    assert_eq!(forked.model, "glm-5.3");

    server.shutdown_gracefully().await?;
    let mut restarted = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;
    let resume_id = restarted
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: started.thread.id,
            ..Default::default()
        })
        .await?;
    let resumed: ThreadResumeResponse =
        timeout(READ_TIMEOUT, restarted.read_response(resume_id)).await??;
    assert_eq!(resumed.model_provider, "opencode-go");
    assert_eq!(resumed.model, "glm-5.3");
    Ok(())
}
