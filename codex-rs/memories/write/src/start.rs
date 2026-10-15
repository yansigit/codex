use crate::ensure_layout;
use crate::extensions::seed_extension_instructions;
use crate::guard;
use crate::metrics::MEMORY_STARTUP;
use crate::phase1;
use crate::phase2;
use crate::runtime::MemoryStartupContext;
use codex_core::CodexThread;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_features::Feature;
use codex_login::AuthManager;
use codex_protocol::MemoryVersion;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::SessionSource;
use std::sync::Arc;
use tracing::warn;

/// Starts the asynchronous startup memory pipeline for an eligible root session.
///
/// The pipeline is skipped for ephemeral sessions, disabled feature flags, and
/// subagent sessions.
pub fn start_memories_startup_task(
    thread_manager: Arc<ThreadManager>,
    auth_manager: Arc<AuthManager>,
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    config: Arc<Config>,
    parent_permission_profile: PermissionProfile,
    source: &SessionSource,
) {
    if config.ephemeral
        || !config.features.enabled(Feature::MemoryTool)
        || source.is_non_root_agent()
    {
        return;
    }

    let versions = if config.memories.dual_write {
        vec![MemoryVersion::V1, MemoryVersion::V2]
    } else {
        vec![config.memories.version]
    };
    for version in versions {
        let mut pipeline_config = config.as_ref().clone();
        pipeline_config.memories.version = version;
        let config = Arc::new(pipeline_config);
        let auth_manager = Arc::clone(&auth_manager);
        let parent_permission_profile = parent_permission_profile.clone();
        let context = Arc::new(MemoryStartupContext::new(
            Arc::clone(&thread_manager),
            Arc::clone(&auth_manager),
            thread_id,
            Arc::clone(&thread),
            config.as_ref(),
            source.clone(),
        ));
        tokio::spawn(async move {
            if context.memory_store().await.is_none() {
                warn!("state db unavailable for memories startup pipeline; skipping");
                return;
            }
            let root = config
                .codex_home
                .join(config.memories.version.directory_name());
            if let Err(err) = ensure_layout(&root).await {
                warn!("failed preparing memories root: {err}");
                return;
            }
            if let Err(err) = seed_extension_instructions(&root).await {
                warn!("failed seeding memory extension instructions: {err}");
            }

            // Clean memories to make preserve DB size. This does not consume tokens so can be
            // done before the quota check.
            phase1::prune(context.as_ref(), &config).await;

            if !guard::rate_limits_ok(&auth_manager, &config).await {
                context.counter(
                    MEMORY_STARTUP,
                    /*inc*/ 1,
                    &[("status", "skipped_rate_limit")],
                );
                return;
            }

            // Run phase 1.
            phase1::run(Arc::clone(&context), Arc::clone(&config)).await;
            // Run phase 2.
            phase2::run(context, config, parent_permission_profile).await;
        });
    }
}

#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostMemoryStatus {
    pub active: bool,
    pub phase: String,
    pub stop_requested: bool,
    pub force_stop: bool,
    pub tokens_used: i64,
    pub child_thread_id: Option<String>,
    pub error: Option<String>,
}
static HOST_JOB: std::sync::Mutex<Option<HostMemoryStatus>> = std::sync::Mutex::new(None);
pub fn host_status() -> HostMemoryStatus {
    HOST_JOB
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .unwrap_or_default()
}
fn host_update(f: impl FnOnce(&mut HostMemoryStatus)) {
    if let Some(state) = HOST_JOB
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_mut()
    {
        f(state);
    }
}
pub fn host_stop(force: bool) -> HostMemoryStatus {
    host_update(|s| {
        s.stop_requested = true;
        s.force_stop |= force;
    });
    host_status()
}
pub(crate) fn host_tokens(tokens: i64) {
    host_update(|s| s.tokens_used = s.tokens_used.saturating_add(tokens.max(0)));
}
pub(crate) fn host_error(reason: &str) {
    host_update(|s| s.error = Some(reason.to_string()));
}
pub(crate) fn host_child(id: Option<ThreadId>) {
    host_update(|s| s.child_thread_id = id.map(|v| v.to_string()));
}
pub(crate) async fn host_force_stop_requested() {
    loop {
        if host_status().force_stop {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[allow(clippy::too_many_arguments)]
pub fn start_host_memory_task(
    thread_manager: Arc<ThreadManager>,
    auth_manager: Arc<AuthManager>,
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    config: Arc<Config>,
    permission: PermissionProfile,
    source: SessionSource,
    store: codex_state::MemoryStore,
) -> Result<(), String> {
    if !codex_extension_api::host_memory_admitted()
        || !config.ephemeral
        || source.is_non_root_agent()
    {
        return Err("host memory maintenance requires an admitted ephemeral root".to_string());
    }
    {
        let mut job = HOST_JOB
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if job.as_ref().is_some_and(|s| s.active) {
            return Err("memory maintenance is already active".to_string());
        }
        *job = Some(HostMemoryStatus {
            active: true,
            phase: "preparing".to_string(),
            ..Default::default()
        });
    }
    let mut config = config.as_ref().clone();
    config.memories.dual_write = false;
    config.memories.max_rollouts_per_startup = 1;
    config.memories.extract_model = config.model.clone();
    config.memories.consolidation_model = config.model.clone();
    let config = Arc::new(config);
    let context = Arc::new(
        MemoryStartupContext::new(
            thread_manager,
            Arc::clone(&auth_manager),
            thread_id,
            thread,
            &config,
            source,
        )
        .with_host_store(store),
    );
    tokio::spawn(async move {
        // A panic with a live internal child leaves admission occupied.
        struct Settled;
        impl Drop for Settled {
            fn drop(&mut self) {
                host_update(|s| {
                    s.active = s.child_thread_id.is_some();
                    s.phase = if s.active {
                        "recovering"
                    } else if s.error.is_some() {
                        "failed"
                    } else if s.stop_requested {
                        "paused"
                    } else {
                        "complete"
                    }
                    .to_string();
                });
            }
        }
        let _settled = Settled;
        if context.memory_store().await.is_none() {
            host_error("native memory state unavailable");
            return;
        }
        let root = config
            .codex_home
            .join(config.memories.version.directory_name());
        if ensure_layout(&root).await.is_err() {
            host_error("native memory layout unavailable");
            return;
        }
        phase1::prune(&context, &config).await;
        if host_status().stop_requested {
            return;
        }
        if !guard::rate_limits_ok(&auth_manager, &config).await {
            host_error("provider rate limit prevents maintenance");
            return;
        }
        host_update(|s| s.phase = "extracting".to_string());
        phase1::run(Arc::clone(&context), Arc::clone(&config)).await;
        if host_status().stop_requested {
            return;
        }
        host_update(|s| s.phase = "consolidating".to_string());
        phase2::run(context, config, permission).await;
    });
    Ok(())
}
