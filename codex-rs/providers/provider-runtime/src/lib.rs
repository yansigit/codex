//! Durable native session orchestration, independent of host protocol types.
mod driver;
pub use driver::drive_session;
mod session;
pub use session::ChannelHost;
pub use session::HostCall;
pub use session::TurnCheckpoint;
pub use session::failed_result_bindings;
pub use session::lineage_hash;
pub use session::validate_lineage;
mod store;
pub use store::ExecutionLease;
pub use store::SessionStore;
pub use store::ToolDisposition;
pub use store::ToolTicket;
