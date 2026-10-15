//! Hosted startup capture and immutable process policy.

use crate::ToolName;
use crate::ToolPolicy;

struct HostState {
    root: ToolPolicy,
    child: ToolPolicy,
    memory_admitted: bool,
    extensions: serde_json::Value,
    resources: serde_json::Value,
}

// An ordinary startup also seals the capture; later environment changes cannot enable it.
static HOST_STATE: std::sync::OnceLock<Option<HostState>> = std::sync::OnceLock::new();
fn host_state() -> Option<&'static HostState> {
    HOST_STATE.get().and_then(Option::as_ref)
}
pub fn host_tool_policy_enabled() -> bool {
    host_state().is_some()
}
pub fn host_memory_admitted() -> bool {
    host_state().is_some_and(|state| state.memory_admitted)
}

fn host_settings_present(
    settings: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> std::io::Result<bool> {
    let mut required = false;
    let mut supplied = false;
    for (name, value) in settings {
        if name == "CODEX_REQUIRE_HOST_POLICY" {
            match value.to_str() {
                Some("1") => required = true,
                Some("0") => {}
                _ => return Err(std::io::Error::other("invalid CODEX_REQUIRE_HOST_POLICY")),
            }
        } else if name.to_string_lossy().starts_with("CODEX_HOST_") {
            supplied = true;
        }
    }
    Ok(required || supplied)
}

pub fn load_host_tool_policy() -> std::io::Result<()> {
    let state = capture_host_state(std::env::vars_os().collect())?;
    if HOST_STATE.get().is_some_and(Option::is_none) && state.is_none() {
        return Ok(());
    }
    HOST_STATE
        .set(state)
        .map_err(|_| std::io::Error::other("host startup already captured"))
}

type Settings = std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>;
fn capture_host_state(settings: Settings) -> std::io::Result<Option<HostState>> {
    if !host_settings_present(settings.clone())? {
        return Ok(None);
    }
    let path = settings
        .get(std::ffi::OsStr::new("CODEX_HOST_TOOL_POLICY"))
        .ok_or_else(|| std::io::Error::other("host tool policy is required"))?;
    let root = parse_host_tool_policy(
        serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?,
    )?;
    let child = match settings.get(std::ffi::OsStr::new("CODEX_HOST_CHILD_TOOL_POLICY")) {
        Some(path) => parse_host_tool_policy(
            serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?,
        )?,
        None => ToolPolicy {
            allowed_tools: Some(vec![]),
            ..ToolPolicy::default()
        },
    };
    let memory_admitted = match settings
        .get(std::ffi::OsStr::new("CODEX_HOST_MEMORY_MAINTENANCE"))
        .map(|value| value.to_str())
    {
        None | Some(Some("0")) => false,
        Some(Some("1")) => true,
        _ => return Err(std::io::Error::other("invalid host memory admission")),
    };
    let extensions = load_host_extensions(&settings)?;
    let resources = load_host_mcp_resources(&settings)?;
    Ok(Some(HostState {
        root,
        child,
        memory_admitted,
        extensions,
        resources,
    }))
}

fn parse_host_tool_policy(value: serde_json::Value) -> std::io::Result<ToolPolicy> {
    if value.as_object().map(serde_json::Map::len) != Some(2) {
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
                    if version == Some(2)
                        && let Some(object) = name.as_object()
                        && object.len() == 2
                        && let (Some(namespace), Some(name)) = (
                            object.get("namespace").and_then(serde_json::Value::as_str),
                            object.get("name").and_then(serde_json::Value::as_str),
                        )
                    {
                        let valid = |part: &str| {
                            !part.is_empty()
                                && part
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                        };
                        if valid(namespace) && valid(name) {
                            return Ok(ToolName::namespaced(namespace, name));
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
    let Some(state) = host_state() else {
        return;
    };
    apply_policy(init, child, state);
}

fn apply_policy(init: &mut crate::ExtensionDataInit, child: bool, state: &HostState) {
    let host = &state.root;
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
                .filter(|tool| state.child.allows(tool))
                .collect(),
        );
    }
    policy.require_managed_sandbox |= host.require_managed_sandbox;
    policy.require_unified_exec |= host.require_unified_exec;
    policy.expose_additional_permissions &= host.expose_additional_permissions;
    init.insert(policy);
}

fn load_host_extensions(settings: &Settings) -> std::io::Result<serde_json::Value> {
    let value: serde_json::Value =
        match settings.get(std::ffi::OsStr::new("CODEX_HOST_EXTENSION_SNAPSHOT")) {
            Some(path) => {
                serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?
            }
            None => serde_json::json!({"version": 1, "mcpServers": {}, "skills": []}),
        };
    if value.as_object().map(serde_json::Map::len) != Some(3)
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
    Ok(value)
}
pub fn host_mcp_servers() -> serde_json::Value {
    host_state()
        .map(|state| &state.extensions)
        .map(|v| v["mcpServers"].clone())
        .unwrap_or_else(|| serde_json::json!({}))
}
pub fn host_skill_roots() -> Vec<String> {
    host_state()
        .map(|state| &state.extensions)
        .and_then(|v| v["skills"].as_array())
        .map(|paths| {
            paths
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn load_host_mcp_resources(settings: &Settings) -> std::io::Result<serde_json::Value> {
    let value: serde_json::Value =
        match settings.get(std::ffi::OsStr::new("CODEX_HOST_MCP_RESOURCE_POLICY")) {
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
    if value.as_object().map(serde_json::Map::len) != Some(3)
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
    Ok(value)
}
pub fn host_mcp_resource_allowed(server: &str, helper: bool) -> bool {
    if !host_tool_policy_enabled() {
        return true;
    }
    host_state()
        .map(|state| &state.resources)
        .and_then(|v| v[if helper { "helper" } else { "root" }].as_array())
        .is_some_and(|items| items.iter().any(|s| s.as_str() == Some(server)))
}

#[cfg(test)]
mod shinyabi_host_policy_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partial_host_settings_require_root_policy() {
        for name in [
            "CODEX_REQUIRE_HOST_POLICY",
            "CODEX_HOST_CHILD_TOOL_POLICY",
            "CODEX_HOST_EXTENSION_SNAPSHOT",
            "CODEX_HOST_MCP_PROXY",
            "CODEX_HOST_MEMORY_MAINTENANCE",
        ] {
            let settings = [(name.into(), "1".into())].into_iter().collect();
            assert!(capture_host_state(settings).is_err());
        }
        assert!(capture_host_state(Settings::new()).unwrap().is_none());
    }

    #[test]
    fn child_intersection_cannot_relax_existing_restrictions() {
        let state = HostState {
            root: parse_host_tool_policy(
                json!({"version":2,"allowed_tools":["exec", {"namespace":"web","name":"run"}]}),
            )
            .unwrap(),
            child: parse_host_tool_policy(json!({"version":2,"allowed_tools":["exec", "write"]}))
                .unwrap(),
            memory_admitted: false,
            extensions: json!({}),
            resources: json!({}),
        };
        let mut init = crate::ExtensionDataInit::default();
        init.insert(ToolPolicy {
            allowed_tools: Some(vec![ToolName::namespaced("web", "run")]),
            ..ToolPolicy::default()
        });
        apply_policy(&mut init, true, &state);
        let policy = init.get::<ToolPolicy>().unwrap();
        assert!(policy.allowed_tools.as_ref().unwrap().is_empty());
        assert!(!policy.expose_additional_permissions);
    }

    #[test]
    fn ordinary_and_required_startup_modes() {
        let settings = |pairs: &[(&str, &str)]| {
            host_settings_present(pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())))
        };
        assert!(!settings(&[]).unwrap());
        assert!(!settings(&[("CODEX_REQUIRE_HOST_POLICY", "0")]).unwrap());
        assert!(settings(&[("CODEX_REQUIRE_HOST_POLICY", "1")]).unwrap());
        assert!(settings(&[("CODEX_HOST_CHILD_TOOL_POLICY", "")]).unwrap());
        assert!(
            settings(&[
                ("CODEX_REQUIRE_HOST_POLICY", "0"),
                ("CODEX_HOST_MCP_PROXY", "proxy")
            ])
            .unwrap()
        );
        assert!(settings(&[("CODEX_REQUIRE_HOST_POLICY", "false")]).is_err());
    }

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
