use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use codex_provider_api::Checkpoint;
use codex_provider_api::Namespace;
use codex_provider_api::ProviderError;
use codex_provider_api::Request;
use codex_provider_api::Result;
use codex_provider_api::ToolCall;
use codex_provider_api::ToolResult;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;

const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Execution {
    pub call: ToolCall,
    pub result: Option<ToolResult>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct State {
    pub conversation: String,
    pub checkpoint: String,
    pub message_count: usize,
    pub system_hash: String,
    pub blobs: BTreeMap<String, String>,
    pub executions: BTreeMap<String, Execution>,
}

pub(crate) fn hash<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| ProviderError::Checkpoint("cannot hash Cursor context".into()))?;
    Ok(format!("{:x}", sha2::Sha256::digest(&bytes)))
}

impl State {
    pub fn restore(request: &Request) -> Result<Self> {
        let Some(checkpoint) = &request.checkpoint else {
            return Ok(Self {
                conversation: uuid::Uuid::new_v4().to_string(),
                message_count: request.messages.len(),
                system_hash: hash(&request.system)?,
                ..Default::default()
            });
        };
        if checkpoint.version != 1 || checkpoint.namespace != request.namespace {
            return Err(ProviderError::Checkpoint(
                "Cursor checkpoint identity or version mismatch".into(),
            ));
        }
        let state: Self = serde_json::from_value(checkpoint.state.clone())
            .map_err(|_| ProviderError::Checkpoint("invalid Cursor checkpoint state".into()))?;
        if state.message_count > request.messages.len()
            || state.system_hash != hash(&request.system)?
            || checkpoint.context_hash != hash(&request.messages[..state.message_count])?
        {
            return Err(ProviderError::Checkpoint(
                "Cursor checkpoint context prefix mismatch".into(),
            ));
        }
        if state
            .executions
            .values()
            .any(|entry| entry.result.is_none())
        {
            return Err(ProviderError::Checkpoint("Cursor checkpoint has an execution with unknown outcome; refusing to execute it again".into()));
        }
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<()> {
        let mut total = STANDARD
            .decode(&self.checkpoint)
            .map_err(|_| ProviderError::Checkpoint("invalid Cursor checkpoint encoding".into()))?
            .len();
        for (id, data) in &self.blobs {
            let id = STANDARD
                .decode(id)
                .map_err(|_| ProviderError::Checkpoint("invalid Cursor blob identifier".into()))?;
            let data = STANDARD
                .decode(data)
                .map_err(|_| ProviderError::Checkpoint("invalid Cursor blob encoding".into()))?;
            if id.is_empty()
                || id.len() > 4096
                || (id.len() == 32 && sha2::Sha256::digest(&data).as_slice() != id)
            {
                return Err(ProviderError::Checkpoint(
                    "Cursor blob hash mismatch".into(),
                ));
            }
            total = total.saturating_add(data.len());
        }
        if total > MAX_STATE_BYTES {
            return Err(ProviderError::Checkpoint(
                "Cursor checkpoint exceeds storage limit".into(),
            ));
        }
        Ok(())
    }

    pub fn put_blob(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        let id = sha2::Sha256::digest(data).to_vec();
        self.set_blob(&id, data)?;
        Ok(id)
    }

    pub fn set_blob(&mut self, id: &[u8], data: &[u8]) -> Result<()> {
        if id.is_empty()
            || id.len() > 4096
            || (id.len() == 32 && sha2::Sha256::digest(data).as_slice() != id)
        {
            return Err(ProviderError::Checkpoint(
                "Cursor blob hash mismatch".into(),
            ));
        }
        let key = STANDARD.encode(id);
        let value = STANDARD.encode(data);
        if let Some(existing) = self.blobs.get(&key) {
            if existing != &value {
                return Err(ProviderError::Checkpoint(
                    "Cursor blob identity collision".into(),
                ));
            }
            return Ok(());
        }
        let total = self.blobs.values().map(String::len).sum::<usize>() + value.len();
        if total > MAX_STATE_BYTES {
            return Err(ProviderError::Checkpoint(
                "Cursor blob store exceeds limit".into(),
            ));
        }
        self.blobs.insert(key, value);
        Ok(())
    }

    pub fn blob(&self, id: &[u8]) -> Result<Option<Vec<u8>>> {
        self.blobs
            .get(&STANDARD.encode(id))
            .map(|data| {
                STANDARD
                    .decode(data)
                    .map_err(|_| ProviderError::Checkpoint("invalid Cursor blob data".into()))
            })
            .transpose()
    }

    pub fn snapshot(&self, namespace: &Namespace, context_hash: &str) -> Result<Checkpoint> {
        let encoded = serde_json::to_vec(self)
            .map_err(|_| ProviderError::Checkpoint("cannot serialize Cursor checkpoint".into()))?;
        if encoded.len() > MAX_STATE_BYTES {
            return Err(ProviderError::Checkpoint(
                "Cursor checkpoint exceeds storage limit".into(),
            ));
        }
        Ok(Checkpoint {
            version: 1,
            namespace: namespace.clone(),
            context_hash: context_hash.to_string(),
            state: serde_json::to_value(self).map_err(|_| {
                ProviderError::Checkpoint("cannot serialize Cursor checkpoint".into())
            })?,
        })
    }
}
