//! Durable native session orchestration, independent of host protocol types.
mod driver;
pub use driver::drive_session;
mod session;
pub use session::{
    ChannelHost, HostCall, TurnCheckpoint, failed_result_bindings, lineage_hash, validate_lineage,
};
mod store;
pub use store::{ExecutionLease, SessionStore, ToolDisposition, ToolTicket};
