//! Reports v2 readiness without exposing memory contents or persistence details to clients.

use super::*;
use codex_app_server_protocol::MemoryStatusParams;
use codex_app_server_protocol::MemoryStatusResponse;
use codex_protocol::MemoryVersion;

impl ThreadRequestProcessor {
    pub(crate) async fn memory_status(
        &self,
        params: MemoryStatusParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let minimum = params.min_consolidated_threads.unwrap_or(20);
        if !(1..=4096).contains(&minimum) {
            return Err(invalid_params(
                "minConsolidatedThreads must be between 1 and 4096",
            ));
        }
        let db = self
            .state_db
            .as_ref()
            .ok_or_else(|| internal_error("sqlite state db unavailable for memory status"))?;
        let store = db
            .memories_for_version(MemoryVersion::V2)
            .await
            .map_err(|error| internal_error(format!("failed to open v2 memory state: {error}")))?;
        let count = store
            .max_consolidated_thread_count()
            .await
            .map_err(|error| internal_error(format!("failed to read memory progress: {error}")))?;
        let root = self
            .config
            .codex_home
            .join(MemoryVersion::V2.directory_name());
        let summary = tokio::fs::read_to_string(root.join("memory_summary.md"))
            .await
            .ok();
        let ready = count >= minimum
            && summary
                .as_deref()
                .is_some_and(codex_memories_write::workspace::is_valid_v2_summary);
        Ok(Some(
            MemoryStatusResponse {
                v2_consolidated_threads: count,
                v2_ready: ready,
            }
            .into(),
        ))
    }
}

impl ThreadRequestProcessor {
    pub(crate) async fn memory_inspect(
        &self,
        params: codex_app_server_protocol::MemoryInspectParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        use codex_app_server_protocol::MemoryInspectEntry;
        use codex_app_server_protocol::MemoryInspectOperation;
        use codex_app_server_protocol::MemoryInspectResponse;
        use codex_memories_extension::ListMemoriesRequest;
        use codex_memories_extension::LocalMemoriesBackend;
        use codex_memories_extension::MemoriesBackend;
        use codex_memories_extension::MemoriesBackendError;
        use codex_memories_extension::MemoryEntryType;
        use codex_memories_extension::ReadMemoryRequest;
        use codex_memories_extension::SearchMatchMode;
        use codex_memories_extension::SearchMemoriesRequest;
        if params.path.as_ref().is_some_and(|v| v.len() > 1000)
            || params.query.as_ref().is_some_and(|v| v.len() > 500)
            || params.cursor.as_ref().is_some_and(|v| v.len() > 1000)
        {
            return Err(invalid_params("memory inspection input is too large"));
        }
        let backend = LocalMemoriesBackend::from_memory_root(
            self.config
                .codex_home
                .join(self.config.memories.version.directory_name())
                .to_path_buf(),
        );
        let error =
            |e: MemoriesBackendError| invalid_params(format!("native memory inspection: {e}"));
        let result = match params.operation {
            MemoryInspectOperation::List => {
                let response = backend
                    .list(ListMemoriesRequest {
                        path: params.path.clone(),
                        cursor: params.cursor,
                        max_results: 100,
                    })
                    .await;
                match response {
                    Ok(value) => MemoryInspectResponse {
                        entries: value
                            .entries
                            .into_iter()
                            .map(|v| MemoryInspectEntry {
                                path: v.path,
                                directory: v.entry_type == MemoryEntryType::Directory,
                                content: None,
                                line: None,
                            })
                            .collect(),
                        next_cursor: value.next_cursor,
                        truncated: value.truncated,
                    },
                    Err(MemoriesBackendError::NotFound { .. }) if params.path.is_none() => {
                        MemoryInspectResponse {
                            entries: vec![],
                            next_cursor: None,
                            truncated: false,
                        }
                    }
                    Err(e) => return Err(error(e)),
                }
            }
            MemoryInspectOperation::Read => {
                let value = backend
                    .read(ReadMemoryRequest {
                        path: params
                            .path
                            .ok_or_else(|| invalid_params("memory path is required"))?,
                        line_offset: 1,
                        max_lines: Some(500),
                        max_tokens: 8000,
                    })
                    .await
                    .map_err(error)?;
                MemoryInspectResponse {
                    entries: vec![MemoryInspectEntry {
                        path: value.path,
                        directory: false,
                        content: Some(value.content),
                        line: Some(value.start_line_number as u32),
                    }],
                    next_cursor: None,
                    truncated: value.truncated,
                }
            }
            MemoryInspectOperation::Search => {
                let value = backend
                    .search(SearchMemoriesRequest {
                        queries: vec![
                            params
                                .query
                                .ok_or_else(|| invalid_params("memory query is required"))?,
                        ],
                        match_mode: SearchMatchMode::Any,
                        path: params.path,
                        cursor: params.cursor,
                        context_lines: 2,
                        case_sensitive: false,
                        normalized: true,
                        max_results: 50,
                    })
                    .await
                    .map_err(error)?;
                MemoryInspectResponse {
                    entries: value
                        .matches
                        .into_iter()
                        .map(|v| MemoryInspectEntry {
                            path: v.path,
                            directory: false,
                            content: Some(v.content),
                            line: Some(v.match_line_number as u32),
                        })
                        .collect(),
                    next_cursor: value.next_cursor,
                    truncated: value.truncated,
                }
            }
        };
        Ok(Some(result.into()))
    }
}

impl ThreadRequestProcessor {
    pub(crate) async fn memory_maintenance(
        &self,
        params: codex_app_server_protocol::MemoryMaintenanceParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        use codex_app_server_protocol::MemoryMaintenanceOperation;
        use codex_app_server_protocol::MemoryMaintenanceResponse;
        use codex_memories_write::start::host_status;
        use codex_memories_write::start::host_stop;
        use codex_memories_write::start::start_host_memory_task;
        if !codex_extension_api::host_memory_admitted() {
            return Err(invalid_params("host has not admitted memory maintenance"));
        }
        match params.operation {
            MemoryMaintenanceOperation::Status => {}
            MemoryMaintenanceOperation::Stop => {
                host_stop(params.force.unwrap_or(false));
            }
            MemoryMaintenanceOperation::Start => {
                let id = ThreadId::from_string(
                    &params
                        .thread_id
                        .ok_or_else(|| invalid_params("maintenance root required"))?,
                )
                .map_err(|_| invalid_params("invalid maintenance root"))?;
                let thread = self
                    .thread_manager
                    .get_thread(id)
                    .await
                    .map_err(|_| invalid_params("maintenance root unavailable"))?;
                let snapshot = thread.config_snapshot().await;
                let config = thread.config().await;
                let store = self
                    .state_db
                    .as_ref()
                    .ok_or_else(|| internal_error("native memory state unavailable"))?
                    .memories_for_version(config.memories.version)
                    .await
                    .map_err(|_| internal_error("native memory state unavailable"))?;
                start_host_memory_task(
                    Arc::clone(&self.thread_manager),
                    Arc::clone(&self.auth_manager),
                    id,
                    Arc::clone(&thread),
                    config,
                    snapshot.permission_profile,
                    snapshot.session_source,
                    store,
                )
                .map_err(invalid_params)?;
            }
        }
        let s = host_status();
        Ok(Some(
            MemoryMaintenanceResponse {
                active: s.active,
                phase: s.phase,
                stop_requested: s.stop_requested,
                force_stop: s.force_stop,
                tokens_used: s.tokens_used,
                child_thread_id: s.child_thread_id,
                error: s.error,
            }
            .into(),
        ))
    }
}
