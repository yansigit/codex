use crate::config_toml::ConfigToml;
use crate::types::RawMcpServerConfig;
use codex_features::FEATURES;
use codex_features::legacy_feature_keys;
use codex_protocol::protocol::GranularApprovalConfig;
use schemars::JsonSchema;
use schemars::Schema;
use schemars::generate::SchemaGenerator;
use schemars::generate::SchemaSettings;
use serde_json::Map;
use serde_json::Value;
use std::path::Path;

/// Determines the conditions under which the user is consulted to approve
/// running the command proposed by Codex.
#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "AskForApproval")]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ConfigAskForApproval {
    /// The model decides when to ask the user for approval.
    OnRequest,

    /// Fine-grained controls for individual approval flows.
    ///
    /// When a field is `true`, commands in that category are allowed. When it
    /// is `false`, those requests are automatically rejected instead of shown
    /// to the user.
    Granular(GranularApprovalConfig),

    /// Never ask the user to approve commands. Failures are immediately returned
    /// to the model, and never escalated to the user for approval.
    Never,
}

/// Schema for the `[features]` map with known + legacy keys only.
pub fn features_schema(schema_gen: &mut SchemaGenerator) -> Schema {
    let mut properties = std::collections::BTreeMap::<String, Schema>::new();
    for feature in FEATURES {
        if feature.id == codex_features::Feature::Artifact {
            continue;
        }
        if feature.id == codex_features::Feature::CodeMode {
            properties.insert(
                feature.key.to_string(),
                schema_gen.subschema_for::<codex_features::FeatureToml<
                    codex_features::CodeModeConfigToml,
                >>(),
            );
            continue;
        }
        if feature.id == codex_features::Feature::CodeModeHost {
            properties.insert(
                feature.key.to_string(),
                schema_gen.subschema_for::<codex_features::FeatureToml<
                    codex_features::CodeModeHostConfigToml,
                >>(),
            );
            continue;
        }
        if feature.id == codex_features::Feature::NonPrefixedMcpToolNames {
            properties.insert(
                feature.key.to_string(),
                schema_gen.subschema_for::<codex_features::FeatureToml<
                    codex_features::NonPrefixedMcpToolNamesConfigToml,
                >>(),
            );
            continue;
        }
        if feature.id == codex_features::Feature::GuardianThreadContext {
            // This setting is already part of the guardianv2 feature table.
            continue;
        }
        if feature.id == codex_features::Feature::GuardianV2 {
            properties.insert(
                feature.key.to_string(),
                schema_gen.subschema_for::<codex_features::FeatureToml<
                    codex_features::GuardianV2ConfigToml,
                >>(),
            );
            continue;
        }
        if feature.id == codex_features::Feature::MultiAgentV2 {
            properties.insert(
                feature.key.to_string(),
                schema_gen.subschema_for::<codex_features::FeatureToml<
                    codex_features::MultiAgentV2ConfigToml,
                >>(),
            );
            continue;
        }
        if feature.id == codex_features::Feature::TokenBudget {
            properties.insert(
                feature.key.to_string(),
                schema_gen.subschema_for::<codex_features::FeatureToml<
                    codex_features::TokenBudgetConfigToml,
                >>(),
            );
            continue;
        }
        if feature.id == codex_features::Feature::ContextManagement {
            properties.insert(
                feature.key.to_string(),
                schema_gen.subschema_for::<codex_features::FeatureToml<
                    codex_features::ContextManagementConfigToml,
                >>(),
            );
            continue;
        }
        if feature.id == codex_features::Feature::RolloutBudget {
            properties.insert(
                feature.key.to_string(),
                schema_gen.subschema_for::<codex_features::FeatureToml<
                    codex_features::RolloutBudgetConfigToml,
                >>(),
            );
            continue;
        }
        if feature.id == codex_features::Feature::CurrentTimeReminder {
            properties.insert(
                feature.key.to_string(),
                schema_gen.subschema_for::<codex_features::FeatureToml<
                    codex_features::CurrentTimeReminderConfigToml,
                >>(),
            );
            continue;
        }
        if feature.id == codex_features::Feature::SleepTool {
            properties.insert(
                feature.key.to_string(),
                schema_gen.subschema_for::<codex_features::FeatureToml<
                    codex_features::SleepToolConfigToml,
                >>(),
            );
            continue;
        }
        if feature.id == codex_features::Feature::AppsMcpPathOverride {
            properties.insert(
                feature.key.to_string(),
                removed_apps_mcp_path_override_schema(schema_gen),
            );
            continue;
        }
        if feature.id == codex_features::Feature::NetworkProxy {
            properties.insert(
                feature.key.to_string(),
                schema_gen.subschema_for::<codex_features::FeatureToml<
                    codex_features::NetworkProxyConfigToml,
                >>(),
            );
            continue;
        }
        properties.insert(feature.key.to_string(), schema_gen.subschema_for::<bool>());
    }
    for legacy_key in legacy_feature_keys() {
        properties.insert(legacy_key.to_string(), schema_gen.subschema_for::<bool>());
    }
    properties.insert(
        "tool_registry".to_string(),
        schema_gen.subschema_for::<codex_features::ToolRegistryConfigToml>(),
    );
    schemars::json_schema!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    })
}

fn removed_apps_mcp_path_override_schema(schema_gen: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "anyOf": [schema_gen.subschema_for::<bool>(), {
            "type": "object",
            "properties": {
                "enabled": schema_gen.subschema_for::<bool>(),
                "path": schema_gen.subschema_for::<String>(),
            },
            "additionalProperties": false,
        }],
    })
}

/// Schema for the `[mcp_servers]` map using the raw input shape.
pub fn mcp_servers_schema(schema_gen: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "object",
        "additionalProperties": schema_gen.subschema_for::<RawMcpServerConfig>(),
    })
}

/// Build the config schema for `config.toml`.
pub fn config_schema() -> Schema {
    let mut schema = SchemaSettings::draft07()
        .with(|settings| {
            settings
                .transforms
                .push(Box::new(codex_protocol::json_schema::sort_required_fields));
            settings
                .transforms
                .push(Box::new(codex_protocol::json_schema::omit_optional_null));
            settings
                .transforms
                .push(Box::new(schemars::transform::RemoveRefSiblings::default()));
        })
        .into_generator()
        .into_root_schema_for::<ConfigToml>();
    add_shell_environment_policy_constraints(&mut schema);
    schema
}

fn add_shell_environment_policy_constraints(schema: &mut Schema) {
    let Some(policy) = schema
        .get_mut("definitions")
        .and_then(|definitions| definitions.get_mut("ShellEnvironmentPolicyToml"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let all_of = policy
        .entry("allOf")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(all_of) = all_of.as_array_mut() {
        for fields in [["exclude", "filters"], ["filters", "include_only"]] {
            all_of.push(serde_json::json!({"not": {"required": fields}}));
        }
    }
}

/// Canonicalize a JSON value by sorting its keys.
pub fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by_key(|(key, _)| *key);
            let mut sorted = Map::with_capacity(map.len());
            for (key, child) in entries {
                sorted.insert(key.clone(), canonicalize(child));
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

/// Render the config schema as pretty-printed JSON.
pub fn config_schema_json() -> anyhow::Result<Vec<u8>> {
    let schema = config_schema();
    let value = serde_json::to_value(schema)?;
    let value = canonicalize(&value);
    let json = serde_json::to_vec_pretty(&value)?;
    Ok(json)
}

/// Write the config schema fixture to disk.
pub fn write_config_schema(out_path: &Path) -> anyhow::Result<()> {
    let json = config_schema_json()?;
    std::fs::write(out_path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn dynamic_config_keys_retain_their_value_schemas() {
        let schema = super::config_schema();
        let definitions = &schema.as_value()["definitions"];
        for (map, value) in [
            ("AgentsToml", "AgentRoleToml"),
            ("AppToolsConfig", "AppToolConfig"),
            ("AppLinksConfig", "AppLinkConfig"),
            ("PluginsConfigToml", "PluginConfig"),
            ("AppsConfigToml", "AppConfig"),
        ] {
            assert_eq!(
                definitions[map]["additionalProperties"]["$ref"],
                format!("#/definitions/{value}"),
                "{map} must accept dynamically named entries"
            );
        }
        assert_eq!(
            definitions["ModelAvailabilityNuxConfig"]["additionalProperties"]["type"],
            "integer"
        );
        assert_eq!(definitions["AgentRoleToml"]["additionalProperties"], false);
    }
}
