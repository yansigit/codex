//! Prepares child configuration from captured step settings and requested overrides.
//!
//! Spawn and reload share live runtime policy; role and model precedence, full-history
//! inheritance, and validation messages remain the same for each multi-agent version.

use crate::agent::role::DEFAULT_ROLE_NAME;
use crate::agent::role::apply_role_to_config;
use crate::config::Config;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use codex_features::Feature;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::models::BaseInstructions;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::protocol::MultiAgentVersion;

pub(crate) const MAX_SPAWN_AGENT_MODEL_OVERRIDES: usize = 5;

pub(crate) fn model_supports_multi_agent_backend(
    model: &ModelPreset,
    multi_agent_version: MultiAgentVersion,
) -> bool {
    multi_agent_version != MultiAgentVersion::V2
        || model.multi_agent_version != Some(MultiAgentVersion::Disabled)
}

/// Selects the existing spawn tool's role-inheritance rules.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnConfigVersion {
    V1,
    V2,
}

pub(crate) struct SpawnConfigOptions<'a> {
    pub(crate) version: SpawnConfigVersion,
    pub(crate) full_history_fork: bool,
    pub(crate) role_name: Option<&'a str>,
    pub(crate) model_provider: Option<&'a str>,
    pub(crate) model: Option<&'a str>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
}

fn validate_host_spawn_overrides(
    hosted: bool,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<(), String> {
    if hosted && (provider.is_some() || model.is_some()) {
        return Err("host subagents must inherit the root provider and model".to_string());
    }
    Ok(())
}

pub(crate) struct PreparedSpawnConfig {
    pub(crate) config: Config,
    pub(crate) role_name: Option<String>,
}

/// Resolves child settings before starting the thread, retaining the invoking tool's precedence.
pub(crate) async fn prepare_agent_spawn_config(
    session: &Session,
    step_context: &StepContext,
    options: SpawnConfigOptions<'_>,
) -> Result<PreparedSpawnConfig, String> {
    validate_host_spawn_overrides(
        codex_extension_api::host_tool_policy_enabled(),
        options.model_provider,
        options.model,
    )?;
    let turn = step_context.turn.as_ref();
    let mut config =
        build_agent_spawn_config(&session.get_base_instructions().await, step_context)?;
    if options.version == SpawnConfigVersion::V1 && options.full_history_fork {
        reject_full_fork_agent_type_override(options.role_name)?;
    }
    let provider_changed = apply_spawn_agent_provider_override(
        &mut config,
        options.model_provider,
        options.model,
        options.full_history_fork,
    )?;
    let models_manager = if provider_changed {
        crate::thread_manager::build_models_manager(&config, session.services.auth_manager.clone())
    } else {
        session.services.models_manager.clone()
    };
    apply_requested_spawn_agent_model_overrides(
        &models_manager,
        step_context,
        &mut config,
        options.model,
        options.reasoning_effort,
        provider_changed,
    )
    .await?;
    if !options.full_history_fork
        || (options.version == SpawnConfigVersion::V2 && options.role_name.is_some())
    {
        apply_spawn_agent_role(&models_manager, &mut config, options.role_name).await?;
        if options.version == SpawnConfigVersion::V2
            && options.full_history_fork
            && config.developer_instructions.is_none()
        {
            config
                .developer_instructions
                .clone_from(&turn.developer_instructions);
        }
    }
    if provider_changed {
        // Roles retain their existing precedence, but cannot silently replace the
        // requested target with a model from the parent's catalog.
        let available = models_manager
            .list_models(RefreshStrategy::Offline, config.http_client_factory())
            .await;
        find_spawn_agent_model_name(
            &available,
            config.model.as_deref().ok_or("child model is missing")?,
            turn.multi_agent_version,
        )?;
    }
    apply_spawn_agent_service_tier_with_manager(session, &models_manager, &mut config).await?;
    apply_spawn_agent_runtime_overrides(&mut config, turn)?;

    // Remember an applied configured default so cold reload reapplies its restrictions.
    let role_name = options
        .role_name
        .or_else(|| {
            (options.version == SpawnConfigVersion::V2
                && !options.full_history_fork
                && config
                    .agent_roles
                    .get(DEFAULT_ROLE_NAME)
                    .is_some_and(|role| role.config_file.is_some()))
            .then_some(DEFAULT_ROLE_NAME)
        })
        .map(str::to_owned);
    Ok(PreparedSpawnConfig { config, role_name })
}

/// An explicit provider choice uses only the parent's configured provider map.
/// Credentials, account selection, discovery and transport remain provider-owned.
fn apply_spawn_agent_provider_override(
    config: &mut Config,
    requested_provider: Option<&str>,
    requested_model: Option<&str>,
    full_history_fork: bool,
) -> Result<bool, String> {
    let Some(provider_id) = requested_provider else {
        return Ok(false);
    };
    let provider = config
        .model_providers
        .get(provider_id)
        .cloned()
        .ok_or_else(|| {
            format!("Unknown configured model_provider `{provider_id}` for spawn_agent")
        })?;
    if provider_id == config.model_provider_id {
        return Ok(false);
    }
    if full_history_fork {
        return Err("Changing model_provider requires a fresh child: use fork_turns=none (V2) or fork_context=false (V1). Cross-provider history portability is not established.".into());
    }
    if requested_model.is_none_or(|model| model.trim().is_empty()) {
        return Err("Changing model_provider requires an explicit model from that provider".into());
    }
    config.model_provider_id = provider_id.to_owned();
    config.model_provider = provider;
    // The parent catalog, context limits and subagent defaults describe its provider.
    config.model_catalog = None;
    config.model_context_window = None;
    config.model_auto_compact_token_limit = None;
    config.agent_default_subagent_model = None;
    config.agent_default_subagent_reasoning_effort = None;
    Ok(true)
}

/// Builds the base config snapshot for a newly spawned sub-agent.
///
/// The returned config starts from the parent's effective config and then refreshes the
/// model selection and reasoning settings captured for the invoking step, plus the turn's
/// runtime approval policy, sandbox, and cwd. Role-specific overrides are layered
/// after this step; skipping this helper and cloning stale config state directly can send the child
/// agent out with the wrong provider or runtime policy.
pub(crate) fn build_agent_spawn_config(
    base_instructions: &BaseInstructions,
    step_context: &StepContext,
) -> Result<Config, String> {
    let mut config = build_agent_shared_config(step_context.turn.as_ref())?;
    let settings = &step_context.settings;
    config.model = Some(settings.model_info.slug.clone());
    config.model_reasoning_effort = settings.effective_reasoning_effort();
    config.model_reasoning_summary = Some(settings.reasoning_summary);
    config.base_instructions = Some(base_instructions.text.clone());
    config.base_instructions_provenance = base_instructions.provenance.clone();
    Ok(config)
}

pub(crate) fn build_agent_resume_config(turn: &TurnContext) -> Result<Config, String> {
    let mut config = build_agent_shared_config(turn)?;
    // For resume, keep base instructions sourced from rollout/session metadata.
    config.base_instructions = None;
    config.base_instructions_provenance = None;
    Ok(config)
}

fn build_agent_shared_config(turn: &TurnContext) -> Result<Config, String> {
    let base_config = turn.config.clone();
    let mut config = (*base_config).clone();
    if config
        .features
        .enabled(Feature::SubagentDefaultContextLimits)
    {
        config.model_context_window = None;
        config.model_auto_compact_token_limit = None;
    }
    // Preserve activation for history forks without freezing the parent's model-owned prompts.
    // Fresh child startup restores configured preferences from the retained snapshot.
    config.token_budget = turn.configured_token_budget.clone();
    config.model = Some(turn.model_info().slug.clone());
    config.model_provider = turn.provider.info().clone();
    config.model_reasoning_effort = turn
        .reasoning_effort()
        .or(turn.model_info().default_reasoning_level.as_ref())
        .cloned();
    config.model_reasoning_summary = Some(turn.reasoning_summary());
    config.developer_instructions = turn.developer_instructions.clone();
    if turn.multi_agent_version == MultiAgentVersion::V2
        && let Some(developer_instructions) = turn
            .config
            .multi_agent_v2
            .subagent_developer_instructions
            .clone()
    {
        config.developer_instructions = Some(developer_instructions);
    }
    apply_spawn_agent_runtime_overrides(&mut config, turn)?;

    Ok(config)
}

fn reject_full_fork_agent_type_override(agent_type: Option<&str>) -> Result<(), String> {
    if agent_type.is_some() {
        return Err(
            "Full-history forked agents inherit the parent agent type; omit agent_type, or spawn without a full-history fork.".to_string());
    }
    Ok(())
}

/// Copies runtime-only turn state onto a child config before it is handed to `AgentControl`.
///
/// These values are chosen by the live turn rather than persisted config, so leaving them stale can
/// make a child agent disagree with its parent about approval policy, cwd, or sandboxing.
fn apply_spawn_agent_runtime_overrides(
    config: &mut Config,
    turn: &TurnContext,
) -> Result<(), String> {
    config
        .permissions
        .approval_policy
        .set(turn.approval_policy())
        .map_err(|err| format!("approval_policy is invalid: {err}"))?;
    config.approvals_reviewer = turn.config.approvals_reviewer;
    #[allow(deprecated)]
    let turn_cwd = turn.cwd.clone();
    config.cwd = turn_cwd;
    config
        .permissions
        .set_permission_profile_from_session_snapshot(
            turn.config
                .permissions
                .permission_profile_state()
                .snapshot(),
        )
        .map_err(|err| format!("permission_profile is invalid: {err}"))?;
    Ok(())
}

async fn apply_requested_spawn_agent_model_overrides(
    models_manager: &SharedModelsManager,
    step_context: &StepContext,
    config: &mut Config,
    requested_model: Option<&str>,
    requested_reasoning_effort: Option<ReasoningEffort>,
    provider_changed: bool,
) -> Result<(), String> {
    let turn = step_context.turn.as_ref();
    let requested_model = requested_model.or(config.agent_default_subagent_model.as_deref());
    let requested_reasoning_effort = requested_reasoning_effort
        .or_else(|| config.agent_default_subagent_reasoning_effort.clone());
    if requested_model.is_none() && requested_reasoning_effort.is_none() {
        return Ok(());
    }

    if let Some(requested_model) = requested_model {
        let available_models = models_manager
            .list_models(
                if provider_changed {
                    RefreshStrategy::OnlineIfUncached
                } else {
                    RefreshStrategy::Offline
                },
                config.http_client_factory(),
            )
            .await;
        let selected_model_name = find_spawn_agent_model_name(
            &available_models,
            requested_model,
            turn.multi_agent_version,
        )?;
        let selected_model_info = models_manager
            .get_model_info(&selected_model_name, &config.to_models_manager_config())
            .await;

        config.model = Some(selected_model_name.clone());
        if let Some(reasoning_effort) = requested_reasoning_effort {
            validate_spawn_agent_reasoning_effort(
                &selected_model_name,
                &selected_model_info.supported_reasoning_levels,
                &reasoning_effort,
            )?;
            config.model_reasoning_effort = Some(reasoning_effort);
        } else {
            config.model_reasoning_effort = selected_model_info.default_reasoning_level;
        }

        return Ok(());
    }

    if let Some(reasoning_effort) = requested_reasoning_effort {
        validate_spawn_agent_reasoning_effort(
            &step_context.settings.model_info.slug,
            &step_context.settings.model_info.supported_reasoning_levels,
            &reasoning_effort,
        )?;
        config.model_reasoning_effort = Some(reasoning_effort);
    }

    Ok(())
}

#[cfg(test)]
pub(crate) async fn apply_spawn_agent_service_tier(
    session: &Session,
    config: &mut Config,
) -> Result<(), String> {
    let models_manager = if config.model_provider != session.get_config().await.model_provider {
        crate::thread_manager::build_models_manager(config, session.services.auth_manager.clone())
    } else {
        session.services.models_manager.clone()
    };
    apply_spawn_agent_service_tier_with_manager(session, &models_manager, config).await
}

async fn apply_spawn_agent_service_tier_with_manager(
    session: &Session,
    models_manager: &SharedModelsManager,
    config: &mut Config,
) -> Result<(), String> {
    let Some(service_tier) = session.services.agent_control.service_tier() else {
        config.service_tier = None;
        return Ok(());
    };
    if service_tier == SERVICE_TIER_DEFAULT_REQUEST_VALUE {
        config.service_tier = Some(service_tier);
        return Ok(());
    }

    let model = config.model.clone().ok_or_else(|| {
        "spawn_agent could not resolve the child model for service tier validation".to_string()
    })?;
    let model_info = models_manager
        .get_model_info(model.as_str(), &config.to_models_manager_config())
        .await;

    config.service_tier = model_info
        .supports_service_tier(service_tier.as_str())
        .then_some(service_tier);
    Ok(())
}

async fn apply_spawn_agent_role(
    models_manager: &SharedModelsManager,
    config: &mut Config,
    role_name: Option<&str>,
) -> Result<(), String> {
    let previous_model = config.model.clone();
    let previous_reasoning_effort = config.model_reasoning_effort.clone();
    apply_role_to_config(config, role_name).await?;
    if config.model == previous_model && config.model_reasoning_effort == previous_reasoning_effort
    {
        return Ok(());
    }

    let Some(reasoning_effort) = config.model_reasoning_effort.clone() else {
        return Ok(());
    };
    let model = config.model.clone().ok_or_else(|| {
        "spawn_agent could not resolve the child model for reasoning effort validation".to_string()
    })?;
    let model_info = models_manager
        .get_model_info(&model, &config.to_models_manager_config())
        .await;
    if model_info.used_fallback_model_metadata {
        return Ok(());
    }

    validate_spawn_agent_reasoning_effort(
        &model,
        &model_info.supported_reasoning_levels,
        &reasoning_effort,
    )
}

fn find_spawn_agent_model_name(
    available_models: &[ModelPreset],
    requested_model: &str,
    multi_agent_version: MultiAgentVersion,
) -> Result<String, String> {
    available_models
        .iter()
        .find(|model| {
            model.model == requested_model
                && model_supports_multi_agent_backend(model, multi_agent_version)
        })
        .map(|model| model.model.clone())
        .ok_or_else(|| {
            let available = available_models
                .iter()
                .filter(|model| model.show_in_picker)
                .filter(|model| model_supports_multi_agent_backend(model, multi_agent_version))
                .take(MAX_SPAWN_AGENT_MODEL_OVERRIDES)
                .map(|model| model.model.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Unknown model `{requested_model}` for spawn_agent. Available models: {available}"
            )
        })
}

fn validate_spawn_agent_reasoning_effort(
    model: &str,
    supported_reasoning_levels: &[ReasoningEffortPreset],
    requested_reasoning_effort: &ReasoningEffort,
) -> Result<(), String> {
    if supported_reasoning_levels
        .iter()
        .any(|preset| &preset.effort == requested_reasoning_effort)
    {
        return Ok(());
    }

    let supported = supported_reasoning_levels
        .iter()
        .map(|preset| preset.effort.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "Reasoning effort `{requested_reasoning_effort}` is not supported for model `{model}`. Supported reasoning efforts: {supported}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::make_session_and_context;
    use codex_model_provider_info::built_in_model_providers;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    #[test]
    fn hosted_overrides_are_rejected_without_changing_native_routing() {
        assert!(validate_host_spawn_overrides(true, None, None).is_ok());
        assert!(validate_host_spawn_overrides(false, Some("cursor"), Some("model")).is_ok());
        assert!(validate_host_spawn_overrides(true, Some("openai"), None).is_err());
        assert!(validate_host_spawn_overrides(true, None, Some("model")).is_err());
    }

    #[tokio::test]
    async fn provider_change_requires_known_provider_explicit_model_and_fresh_history() {
        let (_session, turn) = make_session_and_context().await;
        for (provider, model, fork, expected) in [
            (
                "unknown-provider",
                Some("glm-5.3"),
                false,
                "Unknown configured",
            ),
            ("opencode-go", None, false, "explicit model"),
            ("opencode-go", Some("glm-5.3"), true, "fresh child"),
        ] {
            let mut config = (*turn.config).clone();
            let before = config.clone();
            let error =
                apply_spawn_agent_provider_override(&mut config, Some(provider), model, fork)
                    .expect_err("invalid selection must fail before discovery");
            assert!(error.contains(expected), "{error}");
            assert_eq!(config, before);
        }
    }

    #[test_case::test_case(SpawnConfigVersion::V1; "v1")]
    #[test_case::test_case(SpawnConfigVersion::V2; "v2")]
    #[tokio::test]
    async fn cross_provider_child_uses_target_catalog_and_preserves_parent_policy(
        version: SpawnConfigVersion,
    ) {
        let (session, mut turn) = make_session_and_context().await;
        let mut config = (*turn.config).clone();
        let mut provider = built_in_model_providers(None)["opencode-go"].clone();
        // A configured alias exercises generic resolution without native-ID branches.
        provider.experimental_bearer_token = Some("fixture-token".to_string().into());
        config
            .model_providers
            .insert("target-alias".into(), provider.clone());
        config.model_context_window = Some(123);
        config.model_auto_compact_token_limit = Some(100);
        config.agent_default_subagent_model = Some("parent-only-model".into());
        config.agent_default_subagent_reasoning_effort = Some(ReasoningEffort::None);
        turn.config = Arc::new(config.clone());
        let step = StepContext::for_test(Arc::new(turn));
        let prepared = prepare_agent_spawn_config(
            &session,
            &step,
            SpawnConfigOptions {
                version,
                full_history_fork: false,
                role_name: None,
                model_provider: Some("target-alias"),
                model: Some("glm-5.3"),
                reasoning_effort: None,
            },
        )
        .await
        .expect("target catalog should resolve");
        let child = prepared.config;
        assert_eq!(child.model_provider_id, "target-alias");
        assert_eq!(child.model_provider, provider);
        assert_eq!(child.model.as_deref(), Some("glm-5.3"));
        assert!(child.model_catalog.is_none());
        assert_eq!(child.model_context_window, None);
        assert_eq!(child.model_auto_compact_token_limit, None);
        assert_eq!(child.permissions, config.permissions);
        assert_eq!(child.codex_home, config.codex_home);
        assert_eq!(child.model_providers, config.model_providers);
        assert_eq!(step.turn.config.model_provider_id, config.model_provider_id);
    }

    #[tokio::test]
    async fn cross_provider_child_rejects_a_model_only_known_to_parent() {
        let (session, mut turn) = make_session_and_context().await;
        let mut config = (*turn.config).clone();
        config
            .model_providers
            .get_mut("opencode-go")
            .expect("built-in")
            .experimental_bearer_token = Some("fixture-token".to_string().into());
        turn.config = Arc::new(config);
        let step = StepContext::for_test(Arc::new(turn));
        let result = prepare_agent_spawn_config(
            &session,
            &step,
            SpawnConfigOptions {
                version: SpawnConfigVersion::V2,
                full_history_fork: false,
                role_name: None,
                model_provider: Some("opencode-go"),
                model: Some("parent-only-model"),
                reasoning_effort: None,
            },
        )
        .await;
        assert!(matches!(result, Err(error) if error.contains("Unknown model")));
    }
}
