//! Durable checkpoint, tool journal, and thread execution ownership.
//! Keys and serialized bytes are preserved across the auth/runtime split.
use codex_provider_api::{Checkpoint, Namespace, ProviderError, Result, ToolCall, ToolResult};
use codex_provider_auth::{Store, StoreLock};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio_util::sync::CancellationToken;

#[derive(Serialize, Deserialize)]
struct JournalEntry {
    call: ToolCall,
    result: Option<ToolResult>,
}

pub enum ToolDisposition {
    Execute(ToolTicket),
    Replay(ToolResult),
}
pub struct ToolTicket {
    lock: StoreLock,
    call: ToolCall,
}
impl ToolTicket {
    pub fn finish(self, result: ToolResult) -> Result<ToolResult> {
        if result.call_id != self.call.id {
            return Err(ProviderError::Host("tool result ID mismatch".into()));
        }
        self.lock.write(&JournalEntry {
            call: self.call,
            result: Some(result.clone()),
        })?;
        Ok(result)
    }
}

#[derive(Clone, Debug)]
pub struct SessionStore {
    store: Store,
    namespace: Namespace,
    checkpoint_scope: String,
}

/// Holds the thread execution lock for a whole native request. The durable
/// marker survives process death, model/account changes, and compaction.
pub struct ExecutionLease {
    lock: StoreLock,
}

impl ExecutionLease {
    pub fn mark_tools_started(&self) -> Result<()> {
        self.lock.write(&true)
    }

    pub fn complete(&self) -> Result<()> {
        self.lock.write(&false)
    }
}

impl SessionStore {
    pub fn new(codex_home: &Path, namespace: Namespace) -> Self {
        Self {
            store: Store::new(codex_home),
            namespace,
            checkpoint_scope: "latest".into(),
        }
    }
    /// Context-window identities survive restart and change after compaction.
    /// Only checkpoints are scoped; the tool journal remains shared by the thread.
    pub fn with_checkpoint_scope(mut self, scope: String) -> Self {
        self.checkpoint_scope = scope;
        self
    }
    pub async fn begin_execution(
        &self,
        purpose: &str,
        cancellation: &CancellationToken,
    ) -> Result<ExecutionLease> {
        // Credential and checkpoint data remain provider/account/model scoped.
        // Execution uncertainty belongs to the thread, regardless of the next
        // provider selected. A branch has a new thread identity.
        let key = serde_json::json!(("execution", &self.namespace.thread, purpose)).to_string();
        let lock = self.store.lock(&key, cancellation).await?;
        if lock.read::<bool>()? == Some(true) {
            return Err(ProviderError::Checkpoint(
                "native tool execution was interrupted; inspect its effects and explicitly branch the thread before continuing".into(),
            ));
        }
        Ok(ExecutionLease { lock })
    }
    fn key(&self, kind: &str, id: &str) -> String {
        serde_json::json!((kind, &self.namespace, id)).to_string()
    }
    pub async fn begin_tool(
        &self,
        call: ToolCall,
        cancellation: &CancellationToken,
    ) -> Result<ToolDisposition> {
        let lock = self
            .store
            .lock(&self.key("tool", &call.id), cancellation)
            .await?;
        if let Some(entry) = lock.read::<JournalEntry>()? {
            if entry.call != call {
                return Err(ProviderError::Checkpoint(
                    "tool ID reused with different arguments".into(),
                ));
            }
            return entry.result.map(ToolDisposition::Replay).ok_or_else(|| {
                ProviderError::Checkpoint(
                    "tool execution was interrupted; refusing automatic replay".into(),
                )
            });
        }
        lock.write(&JournalEntry {
            call: call.clone(),
            result: None,
        })?;
        Ok(ToolDisposition::Execute(ToolTicket { lock, call }))
    }
    pub async fn save(
        &self,
        checkpoint: &Checkpoint,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        if checkpoint.version != 1 || checkpoint.namespace != self.namespace {
            return Err(ProviderError::Checkpoint(
                "namespace/version mismatch".into(),
            ));
        }
        let lock = self
            .store
            .lock(
                &self.key("checkpoint", &self.checkpoint_scope),
                cancellation,
            )
            .await?;
        lock.write(checkpoint)
    }
    pub fn load(&self) -> Result<Option<Checkpoint>> {
        let checkpoint = self
            .store
            .read::<Checkpoint>(&self.key("checkpoint", &self.checkpoint_scope))?;
        if checkpoint
            .as_ref()
            .is_some_and(|cp| cp.version != 1 || cp.namespace != self.namespace)
        {
            return Err(ProviderError::Checkpoint(
                "namespace/version mismatch".into(),
            ));
        }
        Ok(checkpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn namespace() -> Namespace {
        Namespace {
            provider: "p".into(),
            account: "a".into(),
            model: "m".into(),
            thread: "t".into(),
        }
    }
    fn call() -> ToolCall {
        ToolCall {
            id: "id".into(),
            name: "shell".into(),
            arguments: json!({"cmd":"test"}),
        }
    }
    #[tokio::test]
    async fn interrupted_execution_blocks_provider_account_model_and_window_changes() {
        let root = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let store = SessionStore::new(root.path(), namespace());
        let execution = store.begin_execution("turn", &cancel).await.unwrap();
        execution.mark_tools_started().unwrap();
        drop(execution);
        let changed = Namespace {
            provider: "other-provider".into(),
            account: "other-account".into(),
            model: "other-model".into(),
            ..namespace()
        };
        let changed_store = SessionStore::new(root.path(), changed.clone())
            .with_checkpoint_scope("compacted-window".into());
        assert!(
            changed_store
                .begin_execution("turn", &cancel)
                .await
                .is_err()
        );
        // Background extraction and explicit branches have separate lifetimes.
        assert!(
            changed_store
                .begin_execution("memory", &cancel)
                .await
                .is_ok()
        );
        let branch = SessionStore::new(
            root.path(),
            Namespace {
                thread: "branch".into(),
                ..changed
            },
        );
        assert!(branch.begin_execution("turn", &cancel).await.is_ok());
    }

    #[tokio::test]
    async fn completed_execution_and_text_only_cancellation_allow_continuation() {
        let root = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let store = SessionStore::new(root.path(), namespace());
        drop(store.begin_execution("turn", &cancel).await.unwrap());
        let execution = store.begin_execution("turn", &cancel).await.unwrap();
        execution.mark_tools_started().unwrap();
        execution.complete().unwrap();
        drop(execution);
        let next = SessionStore::new(
            root.path(),
            Namespace {
                model: "new-model".into(),
                ..namespace()
            },
        );
        assert!(next.begin_execution("turn", &cancel).await.is_ok());
    }

    #[tokio::test]
    async fn interrupted_execution_is_never_silently_replayed() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), namespace());
        let cancel = CancellationToken::new();
        let ticket = store.begin_tool(call(), &cancel).await.unwrap();
        drop(ticket);
        assert!(matches!(
            store.begin_tool(call(), &cancel).await,
            Err(ProviderError::Checkpoint(_))
        ));
    }
    #[tokio::test]
    async fn completed_execution_replays_and_changed_arguments_fail() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), namespace());
        let cancel = CancellationToken::new();
        let ToolDisposition::Execute(ticket) = store.begin_tool(call(), &cancel).await.unwrap()
        else {
            panic!()
        };
        ticket
            .finish(ToolResult {
                call_id: "id".into(),
                output: json!("ok"),
                is_error: false,
            })
            .unwrap();
        assert!(matches!(
            store.begin_tool(call(), &cancel).await.unwrap(),
            ToolDisposition::Replay(_)
        ));
        let mut changed = call();
        changed.arguments = json!({});
        assert!(store.begin_tool(changed, &cancel).await.is_err());
        let mut other = namespace();
        other.account = "other".into();
        assert!(matches!(
            SessionStore::new(root.path(), other)
                .begin_tool(call(), &cancel)
                .await
                .unwrap(),
            ToolDisposition::Execute(_)
        ));
    }
}
