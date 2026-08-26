//! Running an orchestration from the CLI.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use serdes_ai_orchestrator::config::OrchestratorConfig;
use serdes_ai_orchestrator::orchestrator::Orchestrator;
use tokio_util::sync::CancellationToken;

use crate::args::RunMode;
use crate::bus::MessageBus;
use crate::{config, model_factory};

use super::{CliApprover, forward_events};

/// Builds the orchestrator's models the same way the rest of the CLI does.
///
/// The default factory resolves specs through `serdes_ai_models::infer_model`,
/// which knows nothing about the CLI's configuration or its scripted-model
/// fixture. Routing through here keeps a mocked run mocked all the way down —
/// otherwise an orchestration would quietly try to reach a real provider — and
/// lets a fixture script each role separately.
struct CliModelFactory {
    script: Option<Arc<serdes_ai_models::scripted::Script>>,
}

impl CliModelFactory {
    fn new() -> Result<Self> {
        Ok(Self {
            script: model_factory::mock_script()?.map(Arc::new),
        })
    }
}

impl serdes_ai_orchestrator::registry::ModelFactory for CliModelFactory {
    fn model_for(
        &self,
        role: serdes_ai_orchestrator::role::Role,
        spec: &str,
    ) -> std::result::Result<Arc<dyn serdes_ai_models::Model>, serdes_ai_models::ModelError> {
        if let Some(script) = &self.script {
            return Ok(Arc::new(
                serdes_ai_models::scripted::ScriptedModel::for_role(
                    Arc::clone(script),
                    role.as_str(),
                ),
            ));
        }

        model_factory::create_model_sync(spec).map_err(|e| {
            serdes_ai_models::ModelError::configuration(format!(
                "could not build the {role} model: {e}"
            ))
        })
    }
}

/// Execute `task` under `mode`, rendering progress on `bus`.
///
/// Returns the closing summary. [`RunMode::Single`] is not handled here: it is
/// the existing one-agent path and does not involve an orchestrator.
pub async fn run(bus: Arc<MessageBus>, mode: RunMode, task: &str) -> Result<String> {
    if matches!(mode, RunMode::Single) {
        return Err(anyhow!("single mode does not use the orchestrator"));
    }

    let root = std::env::current_dir()?;
    let mut orchestrator_config = OrchestratorConfig::new(root);

    // The CLI's chosen model drives the working agents. The gate keeps its own
    // list, because its independence depends on the verifiers differing from
    // each other rather than matching whatever the session is set to.
    let model = config::get_model_name();
    for role in [
        serdes_ai_orchestrator::role::Role::Orchestrator,
        serdes_ai_orchestrator::role::Role::Code,
        serdes_ai_orchestrator::role::Role::Reviewer,
        serdes_ai_orchestrator::role::Role::Explore,
        serdes_ai_orchestrator::role::Role::Planner,
    ] {
        orchestrator_config = orchestrator_config.with_model(role, model.clone());
    }

    // The gate's verifiers otherwise default to three models from three
    // providers, which needs credentials for all three — unusable for anyone
    // running against a single endpoint.
    if let Some(models) = config::get_gate_verifier_models() {
        orchestrator_config.gate.verifier_models = models;
    }
    if let Some(rounds) = config::get_gate_max_rounds() {
        orchestrator_config.gate.max_rounds = rounds;
    }
    if let Some(command) = config::get_gate_test_command() {
        orchestrator_config.gate.test_command = Some(command);
    }

    let factory = Arc::new(CliModelFactory::new()?);
    let orchestrator = Orchestrator::with_factory(orchestrator_config, factory)
        .map_err(|e| anyhow!("could not start the orchestrator: {e}"))?;

    // Subscribe before starting: events published with no subscriber are dropped,
    // so subscribing afterwards would lose the opening of the run.
    let events = orchestrator.subscribe();
    let renderer = tokio::spawn(forward_events(events, Arc::clone(&bus)));

    let cancel = CancellationToken::new();

    let outcome = match mode {
        RunMode::Fast => orchestrator
            .run_fast(task, cancel)
            .await
            .map_err(|e| anyhow!("{e}")),

        RunMode::Workflow => {
            let approver = CliApprover::new(Arc::clone(&bus));
            orchestrator
                .run_workflow(task, &approver, cancel)
                .await
                .map(|outcome| {
                    if outcome.accepted {
                        format!(
                            "Verified after {} round(s).\n\n{}",
                            outcome.rounds, outcome.summary
                        )
                    } else {
                        // Not an error: the work exists, it just did not convince
                        // the gate. The findings are what the user needs.
                        let findings = outcome
                            .gate
                            .as_ref()
                            .map(|g| g.feedback())
                            .unwrap_or_default();
                        format!(
                            "Not accepted after {} round(s).\n\n{}\n\n{}",
                            outcome.rounds, outcome.summary, findings
                        )
                    }
                })
                .map_err(|e| anyhow!("{e}"))
        }

        RunMode::Single => unreachable!("handled above"),
    };

    // Dropping the orchestrator closes the channel, which ends the renderer.
    drop(orchestrator);
    let _ = renderer.await;

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn single_mode_is_rejected() {
        // Single mode is the existing path; routing it here would silently start
        // an orchestration the user did not ask for.
        let bus = Arc::new(MessageBus::new());

        let err = run(bus, RunMode::Single, "anything").await.unwrap_err();

        assert!(err.to_string().contains("does not use the orchestrator"));
    }
}
