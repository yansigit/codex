use codex_provider_api::{Checkpoint, Namespace, ProviderError, ToolCall, ToolResult};
use codex_provider_auth::{
    AuthStore, LoginFlow, OAuthConfig, SessionStore, Store, TokenRecord, ToolDisposition, digest,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
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

#[tokio::test]
async fn checkpoint_scope_changes_at_compaction_but_interrupted_tool_journal_is_shared() {
    let root = tempfile::tempdir().unwrap();
    let cancellation = CancellationToken::new();
    let namespace = namespace();
    let before_compaction = SessionStore::new(root.path(), namespace.clone())
        .with_checkpoint_scope("window-before-compaction".into());
    let checkpoint = Checkpoint {
        version: 1,
        namespace: namespace.clone(),
        context_hash: "pre-compaction-context".into(),
        state: json!({"cursor":"before"}),
    };
    before_compaction
        .save(&checkpoint, &cancellation)
        .await
        .unwrap();

    let ToolDisposition::Execute(ticket) = before_compaction
        .begin_tool(tool_call(), &cancellation)
        .await
        .unwrap()
    else {
        panic!("first tool execution should start");
    };
    drop(ticket); // Simulate interruption before the result is committed.

    let after_compaction = SessionStore::new(root.path(), namespace)
        .with_checkpoint_scope("window-after-compaction".into());
    assert!(after_compaction.load().unwrap().is_none());
    assert!(matches!(
        after_compaction
            .begin_tool(tool_call(), &cancellation)
            .await,
        Err(ProviderError::Checkpoint(message)) if message.contains("refusing automatic replay")
    ));
}

#[tokio::test]
async fn reused_account_label_gets_new_identity_and_refresh_preserves_generation() {
    let root = tempfile::tempdir().unwrap();
    let store = AuthStore::new(root.path());
    let cancellation = CancellationToken::new();
    let record = |access: &str| TokenRecord {
        access_token: access.into(),
        refresh_token: Some("refresh".into()),
        expires_at: Some(0),
        metadata: json!({"provider":"test"}),
        revoked: false,
    };

    store
        .login("provider-a", "Work", record("first-access"), &cancellation)
        .await
        .unwrap();
    let first_identity = store.identity("provider-a").unwrap().unwrap();
    store
        .login("provider-a", "Work", record("second-access"), &cancellation)
        .await
        .unwrap();
    let second_identity = store.identity("provider-a").unwrap().unwrap();
    assert_ne!(first_identity, second_identity);
    assert_eq!(store.status("provider-a").unwrap().unwrap().account, "Work");

    let refreshed = store
        .credentials_with_refresh(
            "provider-a",
            &cancellation,
            |old, _refresh_cancel| async move {
                Ok(TokenRecord {
                    access_token: "refreshed-access".into(),
                    refresh_token: old.refresh_token,
                    expires_at: None,
                    metadata: old.metadata,
                    revoked: false,
                })
            },
        )
        .await
        .unwrap();
    assert_eq!(refreshed.account_id, second_identity);
}

#[tokio::test]
async fn caller_cancellation_does_not_abort_an_accepted_refresh_rotation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("\"refresh_token\":\"old-refresh\""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(200))
                .set_body_json(json!({
                    "access_token":"rotated-access",
                    "refresh_token":"rotated-refresh",
                    "expires_in":3600
                })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let root = tempfile::tempdir().unwrap();
    let store = AuthStore::new(root.path());
    let record = TokenRecord {
        access_token: "expired-access".into(),
        refresh_token: Some("old-refresh".into()),
        expires_at: Some(0),
        metadata: json!({"tenant":"retained"}),
        revoked: false,
    };
    let initial_cancel = CancellationToken::new();
    store
        .login("provider-a", "Display", record, &initial_cancel)
        .await
        .unwrap();
    let stable_identity = store.identity("provider-a").unwrap().unwrap();

    let caller_cancel = CancellationToken::new();
    let refresh_store = store.clone();
    let refresh_cancel = caller_cancel.clone();
    let refresh_config = config(&server.uri());
    let caller = tokio::spawn(async move {
        refresh_store
            .credentials("provider-a", Some(&refresh_config), &refresh_cancel)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !server.received_requests().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("refresh request was accepted by the mock endpoint");
    caller_cancel.cancel();
    assert!(matches!(
        caller.await.unwrap(),
        Err(ProviderError::Cancelled)
    ));

    let completed = store
        .credentials(
            "provider-a",
            Some(&config(&server.uri())),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(completed.access_token, "rotated-access");
    assert_eq!(completed.account_id, stable_identity);
    assert_eq!(
        store.identity("provider-a").unwrap().unwrap(),
        stable_identity
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn ambiguous_refresh_error_is_persisted_and_never_retried_automatically() {
    let root = tempfile::tempdir().unwrap();
    let store = AuthStore::new(root.path());
    let cancellation = CancellationToken::new();
    store
        .login(
            "provider-a",
            "Display",
            TokenRecord {
                access_token: "expired-access".into(),
                refresh_token: Some("possibly-used".into()),
                expires_at: Some(0),
                metadata: json!({}),
                revoked: false,
            },
            &cancellation,
        )
        .await
        .unwrap();

    let first = store
        .credentials_with_refresh(
            "provider-a",
            &cancellation,
            |_record, _refresh_cancel| async {
                Err(ProviderError::Transport(
                    "connection lost after refresh may have been accepted".into(),
                ))
            },
        )
        .await;
    assert!(matches!(first, Err(ProviderError::Transport(_))));

    let invoked = Arc::new(AtomicBool::new(false));
    let refresh_invoked = Arc::clone(&invoked);
    let second = store
        .credentials_with_refresh(
            "provider-a",
            &cancellation,
            move |_record, _refresh_cancel| async move {
                refresh_invoked.store(true, Ordering::SeqCst);
                Err(ProviderError::Transport("must not run".into()))
            },
        )
        .await;
    assert!(matches!(
        second,
        Err(ProviderError::Authentication(message))
            if message.contains("previous refresh was interrupted")
    ));
    assert!(!invoked.load(Ordering::SeqCst));
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

#[cfg(unix)]
#[test]
fn state_store_rejects_a_symlinked_state_directory_for_reads() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside
            .path()
            .join(format!("{}.json", digest(b"test-state"))),
        b"null",
    )
    .unwrap();
    symlink(outside.path(), root.path().join("native-providers")).unwrap();

    let store = Store::new(root.path());
    assert!(matches!(
        store.read::<serde_json::Value>("test-state"),
        Err(ProviderError::Checkpoint(_))
    ));
}
