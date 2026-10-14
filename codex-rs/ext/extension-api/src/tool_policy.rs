//! A startup ceiling on tool selection and construction, independent of session identity.
//! This policy only restricts tools; permission and approval checks still apply.

use crate::ToolName;

/// Supply through `ExtensionDataInit` before starting a thread. The host captures
/// the policy once; later extension-state changes cannot relax it. Callers must
/// supply it again when resuming a thread.
#[derive(Clone, Debug)]
pub struct ToolPolicy {
    /// `None` keeps ordinary tool selection; an empty list permits no tools.
    /// Names include their namespace; plain names use the default namespace.
    /// Generated tools such as Code Mode's `exec` and `wait` must also be listed.
    pub allowed_tools: Option<Vec<ToolName>>,
    /// Omit core tools unless the thread and every ready environment use a managed sandbox.
    pub require_managed_sandbox: bool,
    /// Omit shell tools when unified exec is disabled, instead of using one-shot exec.
    pub require_unified_exec: bool,
    /// Advertise additional-permission arguments when the feature is enabled.
    pub expose_additional_permissions: bool,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            allowed_tools: None,
            require_managed_sandbox: false,
            require_unified_exec: false,
            expose_additional_permissions: true,
        }
    }
}

impl ToolPolicy {
    pub fn allows(&self, tool: &ToolName) -> bool {
        self.allowed_tools.as_ref().is_none_or(|tools| {
            tools.iter().any(|allowed| {
                allowed.name == tool.name
                    && (allowed.namespace == tool.namespace
                        || (allowed.is_default_namespace() && tool.is_default_namespace()))
            })
        })
    }
}

// Shinyabi host policy: captured once at app-server startup, outside writable homes.
static HOST_TOOL_POLICY: std::sync::OnceLock<ToolPolicy> = std::sync::OnceLock::new();
static HOST_CHILD_TOOL_POLICY: std::sync::OnceLock<ToolPolicy> = std::sync::OnceLock::new();
static HOST_MEMORY_ADMISSION: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn host_tool_policy_enabled() -> bool {
    HOST_TOOL_POLICY.get().is_some()
}
pub fn host_memory_admitted() -> bool {
    HOST_MEMORY_ADMISSION.get() == Some(&true)
}

pub fn load_host_tool_policy() -> std::io::Result<()> {
    load_host_extensions()?;
    load_host_mcp_resources()?;
    HOST_MEMORY_ADMISSION
        .set(std::env::var("CODEX_HOST_MEMORY_MAINTENANCE").as_deref() == Ok("1"))
        .map_err(|_| std::io::Error::other("memory admission already captured"))?;
    let path = std::env::var_os("CODEX_HOST_TOOL_POLICY")
        .ok_or_else(|| std::io::Error::other("host tool policy is required"))?;
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?;
    let host = parse_host_tool_policy(value)?;
    let child = match std::env::var_os("CODEX_HOST_CHILD_TOOL_POLICY") {
        Some(path) => parse_host_tool_policy(
            serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?,
        )?,
        // Older callers cannot accidentally give their process ceiling to a child.
        None => ToolPolicy {
            allowed_tools: Some(vec![]),
            ..ToolPolicy::default()
        },
    };
    HOST_CHILD_TOOL_POLICY
        .set(child)
        .map_err(|_| std::io::Error::other("child host tool policy already captured"))?;
    HOST_TOOL_POLICY
        .set(host)
        .map_err(|_| std::io::Error::other("host tool policy already captured"))
}

fn parse_host_tool_policy(value: serde_json::Value) -> std::io::Result<ToolPolicy> {
    if value.as_object().map(|o| o.len()) != Some(2) {
        return Err(std::io::Error::other("invalid host tool policy fields"));
    }
    let version = value.get("version").and_then(serde_json::Value::as_u64);
    if !matches!(version, Some(1 | 2)) {
        return Err(std::io::Error::other(
            "unsupported host tool policy version",
        ));
    }
    let allowed_tools = match value.get("allowed_tools") {
        Some(serde_json::Value::Array(names)) => Some(
            names
                .iter()
                .map(|name| {
                    if let Some(name) = name.as_str() {
                        if !name.is_empty()
                            && name.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
                            })
                        {
                            return Ok(ToolName::plain(name));
                        }
                        return Err(std::io::Error::other("invalid tool name"));
                    }
                    // Schema 2 makes namespace identity explicit. Dotted strings and
                    // wildcards never authorize a namespaced tool implicitly.
                    if version == Some(2) {
                        if let Some(object) = name.as_object() {
                            if object.len() == 2 {
                                if let (Some(namespace), Some(name)) = (
                                    object.get("namespace").and_then(serde_json::Value::as_str),
                                    object.get("name").and_then(serde_json::Value::as_str),
                                ) {
                                    let valid = |part: &str| {
                                        !part.is_empty()
                                            && part.bytes().all(|b| {
                                                b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
                                            })
                                    };
                                    if valid(namespace) && valid(name) {
                                        return Ok(ToolName::namespaced(namespace, name));
                                    }
                                }
                            }
                        }
                    }
                    Err(std::io::Error::other("invalid tool name"))
                })
                .collect::<std::io::Result<Vec<_>>>()?,
        ),
        _ => return Err(std::io::Error::other("host policy requires allowed_tools")),
    };
    Ok(ToolPolicy {
        allowed_tools,
        expose_additional_permissions: false,
        ..ToolPolicy::default()
    })
}

/// Apply the process ceiling to every new, resumed, forked, or descendant session.
/// Intersect existing restrictions, never replace them with a broader policy.
pub fn apply_host_tool_policy(init: &mut crate::ExtensionDataInit, child: bool) {
    let Some(host) = HOST_TOOL_POLICY.get() else {
        return;
    };
    let mut policy = init
        .get::<ToolPolicy>()
        .map(|p| (*p).clone())
        .unwrap_or_default();
    if let Some(allowed) = &host.allowed_tools {
        policy.allowed_tools = Some(
            allowed
                .iter()
                .filter(|tool| policy.allows(tool))
                .cloned()
                .collect(),
        );
    }
    if child {
        policy.allowed_tools = Some(
            policy
                .allowed_tools
                .unwrap_or_default()
                .into_iter()
                .filter(|tool| {
                    HOST_CHILD_TOOL_POLICY
                        .get()
                        .is_some_and(|child| child.allows(tool))
                })
                .collect(),
        );
    }
    policy.require_managed_sandbox |= host.require_managed_sandbox;
    policy.require_unified_exec |= host.require_unified_exec;
    policy.expose_additional_permissions &= host.expose_additional_permissions;
    init.insert(policy);
}

#[cfg(test)]
mod shinyabi_host_policy_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exact_tool_identities() {
        let policy = parse_host_tool_policy(json!({"version":2,"allowed_tools":[
            "exec", "web.run", {"namespace":"web","name":"run"}
        ]}))
        .unwrap();
        assert!(policy.allows(&ToolName::plain("exec")));
        assert!(policy.allows(&ToolName::namespaced("web", "run")));
        assert!(!policy.allows(&ToolName::plain("run")));
        assert!(!policy.allows(&ToolName::namespaced("other", "run")));
        let legacy =
            parse_host_tool_policy(json!({"version":1,"allowed_tools":["web.run"]})).unwrap();
        assert!(!legacy.allows(&ToolName::namespaced("web", "run")));
    }

    #[test]
    fn malformed_policies_fail_closed() {
        for value in [
            json!(null),
            json!({}),
            json!({"version":3,"allowed_tools":[]}),
            json!({"version":1,"allowed_tools":null}),
            json!({"version":2,"allowed_tools":[""]}),
            json!({"version":2,"allowed_tools":["*"]}),
            json!({"version":2,"allowed_tools":[],"extra":true}),
            json!({"version":1,"allowed_tools":[{"namespace":"web","name":"run"}]}),
            json!({"version":2,"allowed_tools":[{"namespace":"web","name":"*"}]}),
            json!({"version":2,"allowed_tools":[{"namespace":"web","name":"run","extra":true}]}),
        ] {
            assert!(parse_host_tool_policy(value).is_err());
        }
    }
}

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

static HOST_EXTENSION_SNAPSHOT: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
fn load_host_extensions() -> std::io::Result<()> {
    let value: serde_json::Value = match std::env::var_os("CODEX_HOST_EXTENSION_SNAPSHOT") {
        Some(path) => {
            serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?
        }
        None => serde_json::json!({"version": 1, "mcpServers": {}, "skills": []}),
    };
    if value.as_object().map(|v| v.len()) != Some(3)
        || value["version"] != 1
        || !value["mcpServers"].is_object()
        || !value["skills"].as_array().is_some_and(|paths| {
            paths.iter().all(|p| {
                p.as_str()
                    .is_some_and(|p| std::path::Path::new(p).is_absolute())
            })
        })
    {
        return Err(std::io::Error::other("invalid host extension snapshot"));
    }
    HOST_EXTENSION_SNAPSHOT
        .set(value)
        .map_err(|_| std::io::Error::other("host extensions already loaded"))
}
pub fn host_mcp_servers() -> serde_json::Value {
    HOST_EXTENSION_SNAPSHOT
        .get()
        .map(|v| v["mcpServers"].clone())
        .unwrap_or_else(|| serde_json::json!({}))
}
pub fn host_skill_roots() -> Vec<String> {
    HOST_EXTENSION_SNAPSHOT
        .get()
        .and_then(|v| v["skills"].as_array())
        .map(|paths| {
            paths
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

static HOST_MCP_RESOURCES: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
fn load_host_mcp_resources() -> std::io::Result<()> {
    let value: serde_json::Value = match std::env::var_os("CODEX_HOST_MCP_RESOURCE_POLICY") {
        None => serde_json::json!({"version": 1, "root": [], "helper": []}),
        Some(path) => serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|_| std::io::Error::other("invalid MCP resource policy"))?,
    };
    let valid = |key: &str| {
        value[key].as_array().is_some_and(|items| {
            items.len() <= 30
                && items.iter().all(|s| {
                    s.as_str().is_some_and(|s| {
                        !s.is_empty()
                            && s.len() <= 200
                            && s.bytes()
                                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                    })
                })
        })
    };
    if value.as_object().map(|v| v.len()) != Some(3)
        || value["version"] != 1
        || !valid("root")
        || !valid("helper")
        || !value["helper"].as_array().is_some_and(|items| {
            items.iter().all(|s| {
                value["root"]
                    .as_array()
                    .is_some_and(|root| root.contains(s))
            })
        })
    {
        return Err(std::io::Error::other("invalid MCP resource policy"));
    }
    HOST_MCP_RESOURCES
        .set(value)
        .map_err(|_| std::io::Error::other("MCP resource policy already captured"))
}
pub fn host_mcp_resource_allowed(server: &str, helper: bool) -> bool {
    if !host_tool_policy_enabled() {
        return true;
    }
    HOST_MCP_RESOURCES
        .get()
        .and_then(|v| v[if helper { "helper" } else { "root" }].as_array())
        .is_some_and(|items| items.iter().any(|s| s.as_str() == Some(server)))
}
