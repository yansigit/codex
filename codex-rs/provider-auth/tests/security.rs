use codex_provider_api::{Checkpoint, Namespace, ProviderError, ToolCall, ToolResult};
use codex_provider_auth::{
    AuthStore, LoginFlow, OAuthConfig, SessionStore, Store, TokenRecord, ToolDisposition, digest,
};
use serde_json::json;
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;
use url::Url;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(base: &str) -> OAuthConfig {
    OAuthConfig {
        authorization_url: format!("{base}/authorize"),
        token_url: format!("{base}/token"),
        client_id: "public-client".into(),
        redirect_uri: "http://127.0.0.1:53692/callback".into(),
        scopes: vec!["inference".into()],
        authorization_params: BTreeMap::new(),
        token_params: BTreeMap::new(),
        token_json: true,
        exchange_state: true,
    }
}

fn namespace() -> Namespace {
    Namespace {
        provider: "provider-a".into(),
        account: "account-a".into(),
        model: "model-a".into(),
        thread: "thread-a".into(),
    }
}

fn tool_call() -> ToolCall {
    ToolCall {
        id: "call-1".into(),
        name: "shell".into(),
        arguments: json!({"cmd":"true"}),
    }
}

fn callback_state(authorization_url: &str) -> String {
    Url::parse(authorization_url)
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned()
}

#[tokio::test]
async fn duplicate_oauth_state_is_rejected_without_token_exchange() {
    let server = MockServer::start().await;
    let (flow, authorization_url) = LoginFlow::begin(config(&server.uri())).unwrap();
    let state = callback_state(&authorization_url);
    let callback =
        format!("http://127.0.0.1:53692/callback?code=code-1&state={state}&state={state}");

    assert!(
        matches!(flow.finish(&callback, &CancellationToken::new()).await, Err(ProviderError::Authentication(message)) if message == "invalid OAuth state")
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn oauth_success_exchanges_the_verified_code_and_returns_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-result",
            "refresh_token": "refresh-result",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (flow, authorization_url) = LoginFlow::begin(config(&server.uri())).unwrap();
    let state = callback_state(&authorization_url);
    let record = flow
        .finish(
            &format!("http://127.0.0.1:53692/callback?code=valid-code&state={state}"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(record.access_token, "access-result");
    assert_eq!(record.refresh_token.as_deref(), Some("refresh-result"));
    assert!(!record.revoked);
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["grant_type"], "authorization_code");
    assert_eq!(body["code"], "valid-code");
    assert_eq!(body["state"], state);
    assert_eq!(body["client_id"], "public-client");
    assert!(
        body["code_verifier"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
}

#[tokio::test]
async fn refresh_without_rotated_token_keeps_old_token_for_next_refresh() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("\"refresh_token\":\"refresh-stable\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "refreshed",
            "expires_in": 0
        })))
        .expect(2)
        .mount(&server)
        .await;
    let root = tempfile::tempdir().unwrap();
    let store = AuthStore::new(root.path());
    let cancellation = CancellationToken::new();
    store
        .login(
            "provider-a",
            "account-a",
            TokenRecord {
                access_token: "expired".into(),
                refresh_token: Some("refresh-stable".into()),
                expires_at: Some(0),
                metadata: json!({"tenant":"a"}),
                revoked: false,
            },
            &cancellation,
        )
        .await
        .unwrap();
    let config = config(&server.uri());

    assert_eq!(
        store
            .credentials("provider-a", Some(&config), &cancellation)
            .await
            .unwrap()
            .access_token,
        "refreshed"
    );
    assert_eq!(
        store
            .credentials("provider-a", Some(&config), &cancellation)
            .await
            .unwrap()
            .access_token,
        "refreshed"
    );
}

#[tokio::test]
async fn checkpoint_and_tool_journal_are_isolated_by_full_namespace() {
    let root = tempfile::tempdir().unwrap();
    let cancellation = CancellationToken::new();
    let original = namespace();
    let session = SessionStore::new(root.path(), original.clone());
    let checkpoint = Checkpoint {
        version: 1,
        namespace: original.clone(),
        context_hash: "exact-prefix-digest".into(),
        state: json!({"cursor":"opaque"}),
    };
    session.save(&checkpoint, &cancellation).await.unwrap();
    assert_eq!(session.load().unwrap(), Some(checkpoint.clone()));

    for mutate in [
        |ns: &mut Namespace| ns.provider.push_str("-other"),
        |ns: &mut Namespace| ns.account.push_str("-other"),
        |ns: &mut Namespace| ns.model.push_str("-other"),
        |ns: &mut Namespace| ns.thread.push_str("-other"),
    ] {
        let mut other = original.clone();
        mutate(&mut other);
        let other_session = SessionStore::new(root.path(), other.clone());
        assert!(other_session.load().unwrap().is_none());
        assert!(matches!(
            other_session.save(&checkpoint, &cancellation).await,
            Err(ProviderError::Checkpoint(_))
        ));
        assert!(matches!(
            other_session
                .begin_tool(tool_call(), &cancellation)
                .await
                .unwrap(),
            ToolDisposition::Execute(_)
        ));
    }

    let ToolDisposition::Execute(ticket) = session
        .begin_tool(tool_call(), &cancellation)
        .await
        .unwrap()
    else {
        panic!("a new tool call should execute in its namespace");
    };
    ticket
        .finish(ToolResult {
            call_id: "call-1".into(),
            output: json!("done"),
            is_error: false,
        })
        .unwrap();
    assert!(matches!(
        session
            .begin_tool(tool_call(), &cancellation)
            .await
            .unwrap(),
        ToolDisposition::Replay(_)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn state_store_rejects_a_preexisting_symlink_for_state_file() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    let cancellation = CancellationToken::new();
    let lock = store.lock("test-state", &cancellation).await.unwrap();
    drop(lock);
    let state_path = root
        .path()
        .join("native-providers")
        .join(format!("{}.json", digest(b"test-state")));
    let outside = root.path().join("outside.json");
    std::fs::write(&outside, b"{}").unwrap();
    symlink(outside, state_path).unwrap();

    assert!(matches!(
        store.read::<serde_json::Value>("test-state"),
        Err(ProviderError::Checkpoint(_))
    ));
}
