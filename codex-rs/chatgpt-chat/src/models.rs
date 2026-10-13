//! Bounded native catalog metadata. Visibility is not proof of send entitlement.
use crate::ChatError;
use crate::ChatProbe;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone)]
pub struct NativeModel {
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone)]
pub struct ModelLimit {
    pub model: String,
    pub resets_at_ms: Option<i64>,
}

#[derive(Clone)]
pub struct ModelCatalog {
    pub models: Vec<NativeModel>,
    pub default_model: String,
    /// None means the init response did not supply quota information. An empty
    /// list only means no model limit was reported, never unlimited usage.
    pub limits: Option<Vec<ModelLimit>>,
}

impl ChatProbe {
    pub async fn models(&self) -> Result<Vec<NativeModel>, ChatError> {
        let value: Value = self
            .read("models?history_and_training_disabled=false", "models")
            .await?;
        let result = parse_models(&value)?;
        self.check_owner()?;
        Ok(result)
    }
}

pub(crate) fn defaults(value: &Value) -> Result<(String, Option<Vec<ModelLimit>>), ChatError> {
    let model = model_id(value.get("default_model_slug"))?;
    let limits = match value.get("model_limits").filter(|v| !v.is_null()) {
        None => None,
        Some(value) => {
            let entries = value.as_array().ok_or(ChatError::ProtocolChanged)?;
            if entries.len() > 256 {
                return Err(ChatError::ResponseTooLarge);
            }
            let mut seen = HashSet::new();
            let mut limits = Vec::new();
            for entry in entries {
                let model = model_id(entry.get("model_slug"))?;
                if !seen.insert(model.clone()) {
                    return Err(ChatError::ProtocolChanged);
                }
                let resets_at_ms = match entry.get("resets_after").filter(|v| !v.is_null()) {
                    None => None,
                    Some(value) => {
                        let timestamp =
                            text(Some(value), 128)?.ok_or(ChatError::ProtocolChanged)?;
                        let parsed = chrono::DateTime::parse_from_rfc3339(&timestamp)
                            .map_err(|_| ChatError::ProtocolChanged)?
                            .timestamp_millis();
                        if parsed < 0 {
                            return Err(ChatError::ProtocolChanged);
                        }
                        Some(parsed)
                    }
                };
                limits.push(ModelLimit {
                    model,
                    resets_at_ms,
                });
            }
            Some(limits)
        }
    };
    Ok((model, limits))
}

fn parse_models(value: &Value) -> Result<Vec<NativeModel>, ChatError> {
    let entries = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or(ChatError::ProtocolChanged)?;
    if entries.len() > 256 {
        return Err(ChatError::ResponseTooLarge);
    }
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries {
        let id = model_id(entry.get("slug"))?;
        if !seen.insert(id.clone()) {
            return Err(ChatError::ProtocolChanged);
        }
        models.push(NativeModel {
            id,
            title: text(entry.get("title"), 1024)?,
            description: text(entry.get("description"), 4096)?,
        });
    }
    Ok(models)
}

fn model_id(value: Option<&Value>) -> Result<String, ChatError> {
    let id = text(value, 256)?.ok_or(ChatError::ProtocolChanged)?;
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
    {
        return Err(ChatError::ProtocolChanged);
    }
    Ok(id)
}

fn text(value: Option<&Value>, max: usize) -> Result<Option<String>, ChatError> {
    value
        .filter(|value| !value.is_null())
        .map(|value| {
            let text = value
                .as_str()
                .filter(|text| text.len() <= max && !text.chars().any(char::is_control))
                .ok_or(ChatError::ProtocolChanged)?;
            Ok(text.to_owned())
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn catalog_is_metadata_only_and_does_not_imply_model_entitlement() {
        let value = json!({"models":[{"slug":"fixture-model","title":"Fixture","description":"Metadata","capabilities":{"untrusted":"ignored"},"enabled_tools":["browser"]}]});
        let models = parse_models(&value).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "fixture-model");
        assert_eq!(models[0].title.as_deref(), Some("Fixture"));
        assert_eq!(models[0].description.as_deref(), Some("Metadata"));
        for bad in [
            json!({"models":[{"slug":"duplicate"},{"slug":"duplicate"}]}),
            json!({"models":[{"slug":""}]}),
            json!({"models":[{"slug":"okay","title":"control\u{1b}"}]}),
            json!({"models":[{"slug":"okay","description":"x".repeat(4097)}]}),
        ] {
            assert!(parse_models(&bad).is_err());
        }
    }

    #[test]
    fn init_defaults_preserve_unknown_quota_and_validate_reset_times() {
        let (default, limits) = defaults(&json!({"default_model_slug":"fixture-model"})).unwrap();
        assert_eq!(default, "fixture-model");
        assert!(limits.is_none());
        assert!(
            defaults(&json!({"default_model_slug":"fixture-model","model_limits":[]}))
                .unwrap()
                .1
                .unwrap()
                .is_empty()
        );
        let (_, limits) = defaults(&json!({"default_model_slug":"fixture-model","model_limits":[{"model_slug":"limited-model","resets_after":"2026-10-12T06:00:00Z"}]})).unwrap();
        assert_eq!(limits.as_ref().unwrap()[0].model, "limited-model");
        assert!(limits.unwrap()[0].resets_at_ms.is_some());
        assert!(defaults(&json!({"default_model_slug":"fixture-model","model_limits":[{"model_slug":"limited-model","resets_after":"private invalid value"}]})).is_err());
    }
}
