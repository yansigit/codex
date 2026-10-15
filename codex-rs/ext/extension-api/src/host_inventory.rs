//! Bounded host tool inventories, keyed by thread.

use crate::host_tool_policy_enabled;

static HOST_TOOL_INVENTORIES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>,
> = std::sync::OnceLock::new();
pub fn record_host_tool_inventory(thread: String, value: serde_json::Value) {
    if !host_tool_policy_enabled() {
        return;
    }
    if let Ok(mut inventories) = HOST_TOOL_INVENTORIES.get_or_init(Default::default).lock() {
        if inventories.len() >= 256 && !inventories.contains_key(&thread) {
            inventories.pop_first();
        }
        inventories.insert(thread, value);
    }
}
pub fn host_tool_inventory(thread: &str) -> Option<serde_json::Value> {
    HOST_TOOL_INVENTORIES
        .get()?
        .lock()
        .ok()?
        .get(thread)
        .cloned()
}
