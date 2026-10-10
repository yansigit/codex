//! Direct HTTP codecs. No SDK, proxy, or tool executor is involved here.
mod chat;
mod google;
mod messages;
mod replay;
mod responses;
mod session;
mod sse;
pub use chat::ChatCodec;
use codex_provider_api::ProviderError;
use codex_provider_api::Result;
pub use google::GoogleCodec;
pub use messages::MessagesCodec;
pub use replay::restore_checkpoint;
pub use responses::ResponsesCodec;
use serde_json::Value;
pub use session::HttpSession;
pub use session::WireCodec;
pub use session::WireRequest;
pub use session::bounded_json;
pub use sse::SseDecoder;

pub(crate) fn protocol(message: impl Into<String>) -> ProviderError {
    ProviderError::Protocol(message.into())
}
pub(crate) fn string(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| protocol(format!("missing {key}")))
}
pub(crate) fn output_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests;
