//! Native inference boundary. Providers never execute host operations themselves.
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

mod driver;
pub use driver::drive_session;

pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
pub type Result<T> = std::result::Result<T, ProviderError>;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider authentication required: {0}")]
    Authentication(String),
    #[error("provider protocol error: {0}")]
    Protocol(String),
    #[error("provider transport error: {0}")]
    Transport(String),
    #[error("unsupported provider operation: {0}")]
    Unsupported(String),
    #[error("provider operation cancelled")]
    Cancelled,
    #[error("provider checkpoint error: {0}")]
    Checkpoint(String),
    #[error("host operation failed: {0}")]
    Host(String),
}

/// These identities must all match before opaque state may be replayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Namespace {
    pub provider: String,
    pub account: String,
    pub model: String,
    pub thread: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub version: u32,
    pub namespace: Namespace,
    /// Hash of the exact conversation prefix to which this state belongs.
    pub context_hash: String,
    pub state: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    Image {
        url: String,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    /// Provider-specific replay material, never forwarded to another provider.
    Opaque {
        namespace: Namespace,
        value: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output: Value,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub namespace: Namespace,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub parallel_tool_calls: bool,
    pub max_output_tokens: Option<u64>,
    pub reasoning_effort: Option<String>,
    pub output_schema: Option<Value>,
    pub checkpoint: Option<Checkpoint>,
    /// Host extension data. `host_checkpoint_prefix`, when present, is the count
    /// of messages the host verified against its durable checkpoint history.
    /// Only the trusted host may set it; providers may then restore their exact
    /// native transcript despite differences in tool presentation/order.
    pub metadata: Value,
}

/// Credential material deliberately has no Debug/Serialize implementation.
#[derive(Clone)]
pub struct Credentials {
    pub access_token: String,
    pub account_id: String,
    pub metadata: Value,
}

#[derive(Clone)]
pub struct Context {
    pub credentials: Credentials,
    pub cancellation: CancellationToken,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub context_window: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCall(ToolCall),
    /// Replay material associated with generated output (including signatures).
    Checkpoint(Checkpoint),
    Usage(Usage),
    Completed {
        id: String,
    },
}

/// A session remains alive across tool requests and tool-result submission.
/// `next_event` must be cancellation-safe. EOF without Completed is an error.
pub trait ProviderSession: Send {
    fn next_event(&mut self) -> ProviderFuture<'_, Option<Event>>;
    fn submit_tool_result(&mut self, result: ToolResult) -> ProviderFuture<'_, ()>;
    fn cancel(&mut self) -> ProviderFuture<'_, ()>;
}

pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn models(&self, context: Context) -> ProviderFuture<'_, Vec<Model>>;
    fn start(
        &self,
        request: Request,
        context: Context,
    ) -> ProviderFuture<'_, Box<dyn ProviderSession>>;
}

/// Implemented by core. This is the ONLY route to filesystem, shell and MCP.
/// Execution must preserve approvals, sandbox, hooks and normal tool presentation.
pub trait Host: Send + Sync {
    /// Durably journal the call before execution and its result before returning.
    /// Repeated IDs with identical arguments return the stored result. An ID
    /// left pending by interrupted execution must fail instead of running again.
    fn execute(
        &self,
        call: ToolCall,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, ToolResult>;
    /// Persist a checkpoint before acknowledging its receipt.
    fn checkpoint(&self, checkpoint: Checkpoint) -> ProviderFuture<'_, ()>;
}

#[derive(Default)]
pub struct Registry {
    providers: std::collections::BTreeMap<&'static str, Arc<dyn Provider>>,
}

impl Registry {
    pub fn register(&mut self, provider: Arc<dyn Provider>) -> Result<()> {
        let id = provider.id();
        if self.providers.contains_key(id) {
            return Err(ProviderError::Protocol(format!("duplicate provider: {id}")));
        }
        self.providers.insert(id, provider);
        Ok(())
    }
    pub fn get(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(id).cloned()
    }
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().copied()
    }
}
