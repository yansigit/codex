use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use codex_provider_api::{Checkpoint, Namespace, ProviderError, Result, ToolCall, ToolResult};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

fn io_error(_: impl std::fmt::Display) -> ProviderError {
    ProviderError::Checkpoint("unable to read or persist private provider state".into())
}

pub fn digest(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

/// Files are addressed by hashes of structured keys, never by remote path input.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(codex_home: &Path) -> Self {
        Self {
            root: codex_home.join("native-providers"),
        }
    }

    fn directory(&self) -> Result<()> {
        if let Ok(meta) = fs::symlink_metadata(&self.root)
            && (meta.file_type().is_symlink() || !meta.is_dir())
        {
            return Err(io_error("unsafe directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(&self.root).map_err(io_error)?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&self.root).map_err(io_error)?;
        Ok(())
    }

    fn path(&self, key: &str, extension: &str) -> PathBuf {
        self.root
            .join(format!("{}.{}", digest(key.as_bytes()), extension))
    }

    pub async fn lock(&self, key: &str, cancellation: &CancellationToken) -> Result<StoreLock> {
        self.directory()?;
        let path = self.path(key, "lock");
        if fs::symlink_metadata(&path).is_ok_and(|meta| !meta.is_file()) {
            return Err(io_error("unsafe lock file"));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path).map_err(io_error)?;
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if cancellation.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            match file.try_lock() {
                Ok(()) => {
                    return Ok(StoreLock {
                        file,
                        store: self.clone(),
                        key: key.to_owned(),
                    });
                }
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    tokio::select! {
                        _ = cancellation.cancelled() => return Err(ProviderError::Cancelled),
                        _ = tokio::time::sleep(Duration::from_millis(20)) => {}
                    }
                }
                Err(_) => return Err(io_error("lock failed")),
            }
        }
    }

    pub fn read<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        if fs::symlink_metadata(&self.root)
            .is_ok_and(|meta| meta.file_type().is_symlink() || !meta.is_dir())
        {
            return Err(io_error("unsafe state directory"));
        }
        let path = self.path(key, "json");
        if fs::symlink_metadata(&path).is_ok_and(|meta| !meta.is_file()) {
            return Err(io_error("unsafe state file"));
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        match options.open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(64 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(io_error)?;
                if bytes.len() > 64 * 1024 * 1024 {
                    return Err(io_error("state exceeds size limit"));
                }
                serde_json::from_slice(&bytes).map(Some).map_err(io_error)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(error)),
        }
    }
}

/// The stable lock file is never removed, including during logout.
pub struct StoreLock {
    file: File,
    store: Store,
    key: String,
}

impl StoreLock {
    pub fn read<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        self.store.read(&self.key)
    }
    pub fn write<T: Serialize>(&self, value: &T) -> Result<()> {
        let mut temp = tempfile::NamedTempFile::new_in(&self.store.root).map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temp.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(io_error)?;
        }
        serde_json::to_writer(temp.as_file_mut(), value).map_err(io_error)?;
        if temp.as_file().metadata().map_err(io_error)?.len() > 64 * 1024 * 1024 {
            return Err(io_error("state exceeds size limit"));
        }
        temp.as_file_mut().flush().map_err(io_error)?;
        temp.as_file().sync_all().map_err(io_error)?;
        temp.persist(self.store.path(&self.key, "json"))
            .map_err(io_error)?;
        #[cfg(unix)]
        File::open(&self.store.root)
            .and_then(|dir| dir.sync_all())
            .map_err(io_error)?;
        Ok(())
    }
    pub fn delete(&self) -> Result<()> {
        match fs::remove_file(self.store.path(&self.key, "json")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(error)),
        }
    }
}
impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

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
