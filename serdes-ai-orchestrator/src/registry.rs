//! Building agents for roles.
//!
//! Models are obtained through a [`ModelFactory`] rather than constructed
//! directly, so tests can run whole orchestrations against mock models with no
//! network access.

use std::sync::Arc;

use serdes_ai_agent::{Agent, AgentBuilder, UsageLimits};
use serdes_ai_models::Model;
use serdes_ai_models::ModelError;

use crate::config::OrchestratorConfig;
use crate::role::Role;
use crate::tools::{self, ToolContext};

/// Supplies the model a role should run on.
pub trait ModelFactory: Send + Sync {
    /// Build the model for `role`, given the configured specification.
    fn model_for(&self, role: Role, spec: &str) -> Result<Arc<dyn Model>, ModelError>;
}

/// Resolves the configured specification string through `serdes-ai-models`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SpecModelFactory;

impl ModelFactory for SpecModelFactory {
    fn model_for(&self, _role: Role, spec: &str) -> Result<Arc<dyn Model>, ModelError> {
        serdes_ai_models::infer_model(spec)
    }
}

/// Bound a single agent run to `max_requests` model calls.
fn request_limit(max_requests: u32) -> UsageLimits {
    UsageLimits {
        max_requests: Some(max_requests),
        ..UsageLimits::default()
    }
}

/// Builds agents for roles.
///
/// Agents are constructed per call rather than cached: an agent carries no
/// per-run state worth reusing, and building one is cheap next to a model
/// request. Caching would also make the registry harder to share across the
/// concurrent subagents that `spawn_agent` starts.
pub struct AgentRegistry {
    config: OrchestratorConfig,
    factory: Arc<dyn ModelFactory>,
    tools: ToolContext,
}

impl AgentRegistry {
    /// Create a registry that resolves models from their configured specs.
    pub fn new(config: OrchestratorConfig) -> Self {
        Self::with_factory(config, Arc::new(SpecModelFactory))
    }

    /// Create a registry backed by a custom [`ModelFactory`].
    pub fn with_factory(config: OrchestratorConfig, factory: Arc<dyn ModelFactory>) -> Self {
        let tools = ToolContext::new(config.root.clone());
        Self {
            config,
            factory,
            tools,
        }
    }

    /// The configuration this registry was built from.
    pub fn config(&self) -> &OrchestratorConfig {
        &self.config
    }

    /// Build a text-output agent for `role`, carrying the tools that role is
    /// permitted to use.
    ///
    /// Roles with a structured output type — the planner and the gate's
    /// verifiers — are built by their own call sites, which know the concrete
    /// `Output`. Keeping them out of the registry avoids having to erase
    /// heterogeneous `Agent<_, Output>` types behind a trait object for no
    /// benefit, since only [`Role::spawnable`] roles are reached through here.
    pub fn build(&self, role: Role) -> Result<Agent<(), String>, ModelError> {
        let role_config = self.config.role(role);
        let model = self.factory.model_for(role, &role_config.model)?;

        let mut builder = AgentBuilder::<(), String>::from_arc(model)
            .system_prompt(role_config.system_prompt)
            .usage_limits(request_limit(role_config.max_requests));

        if let Some(max_tokens) = role_config.max_tokens {
            builder = builder.max_tokens(max_tokens);
        }

        Ok(tools::register_for_role(builder, role, self.tools.clone()).build())
    }

    /// A builder for a role, without tools or an output type applied.
    ///
    /// Used by the planner and verifiers, which need a structured output type and
    /// so cannot go through [`AgentRegistry::build`].
    pub fn builder_for(&self, role: Role) -> Result<AgentBuilder<(), String>, ModelError> {
        let model = self.config.role(role).model;
        self.builder_with_model(role, &model)
    }

    /// A builder for `role` running on an explicitly chosen model.
    ///
    /// The gate uses this to put a different model behind each verifier, which is
    /// where its independence comes from.
    pub fn builder_with_model(
        &self,
        role: Role,
        model_spec: &str,
    ) -> Result<AgentBuilder<(), String>, ModelError> {
        let role_config = self.config.role(role);
        let model = self.factory.model_for(role, model_spec)?;

        Ok(AgentBuilder::<(), String>::from_arc(model)
            .system_prompt(role_config.system_prompt)
            .usage_limits(request_limit(role_config.max_requests)))
    }

    /// The tool context shared by every agent this registry builds.
    pub fn tool_context(&self) -> &ToolContext {
        &self.tools
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serdes_ai_core::messages::StreamCompleteEvent;
    use serdes_ai_core::{
        FinishReason, ModelResponse, ModelResponsePart, ModelResponseStreamEvent,
    };
    use serdes_ai_models::FunctionModel;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A factory that hands out canned models and records what was asked for.
    pub(crate) struct MockFactory {
        replies: HashMap<Role, String>,
        pub(crate) requested: Mutex<Vec<(Role, String)>>,
    }

    impl MockFactory {
        pub(crate) fn new() -> Self {
            Self {
                replies: HashMap::new(),
                requested: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn replying(mut self, role: Role, text: &str) -> Self {
            self.replies.insert(role, text.to_string());
            self
        }
    }

    impl ModelFactory for MockFactory {
        fn model_for(&self, role: Role, spec: &str) -> Result<Arc<dyn Model>, ModelError> {
            self.requested
                .lock()
                .unwrap()
                .push((role, spec.to_string()));

            let reply = self
                .replies
                .get(&role)
                .cloned()
                .unwrap_or_else(|| format!("{role} reply"));

            Ok(Arc::new(streaming_model(reply)))
        }
    }

    /// A model that answers with `reply` on both the blocking and the streaming
    /// path.
    ///
    /// `FunctionModel::constant_text` supplies only the blocking path, so an
    /// agent driven through `run_stream` fails against it. Every agent in an
    /// orchestration is streamed, so the mock has to answer both.
    pub(crate) fn streaming_model(reply: String) -> FunctionModel {
        let blocking = reply.clone();

        FunctionModel::with_both(
            move |_, _| {
                ModelResponse::text(blocking.clone()).with_finish_reason(FinishReason::Stop)
            },
            move |_, _| {
                let events = vec![
                    Ok(ModelResponseStreamEvent::part_start(
                        0,
                        ModelResponsePart::text(""),
                    )),
                    Ok(ModelResponseStreamEvent::text_delta(0, reply.clone())),
                    Ok(ModelResponseStreamEvent::StreamComplete(
                        StreamCompleteEvent::new(FinishReason::Stop),
                    )),
                ];
                Box::pin(futures::stream::iter(events))
            },
        )
    }

    fn registry() -> AgentRegistry {
        AgentRegistry::with_factory(OrchestratorConfig::new("."), Arc::new(MockFactory::new()))
    }

    #[test]
    fn builds_an_agent_for_every_role() {
        let reg = registry();

        for role in Role::ALL {
            assert!(reg.build(role).is_ok(), "failed to build {role}");
        }
    }

    #[test]
    fn built_agents_carry_their_roles_tools() {
        let reg = registry();

        let code = reg.build(Role::Code).unwrap();
        let names: Vec<&str> = code.tools().iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"write_file"));

        let explore = reg.build(Role::Explore).unwrap();
        let names: Vec<&str> = explore.tools().iter().map(|d| d.name.as_str()).collect();
        assert!(!names.contains(&"write_file"));
        assert!(!names.contains(&"bash"));
    }

    #[test]
    fn each_role_is_built_from_its_configured_model() {
        let factory = Arc::new(MockFactory::new());
        let reg = AgentRegistry::with_factory(
            OrchestratorConfig::new(".").with_model(Role::Code, "openai:gpt-5.1"),
            factory.clone(),
        );

        reg.build(Role::Code).unwrap();
        reg.build(Role::Explore).unwrap();

        let asked = factory.requested.lock().unwrap().clone();
        assert!(asked.contains(&(Role::Code, "openai:gpt-5.1".to_string())));
        assert!(asked.contains(&(Role::Explore, "anthropic:claude-haiku-4-5".to_string())));
    }

    #[tokio::test]
    async fn a_built_agent_runs() {
        let reg = AgentRegistry::with_factory(
            OrchestratorConfig::new("."),
            Arc::new(MockFactory::new().replying(Role::Explore, "found it")),
        );

        let agent = reg.build(Role::Explore).unwrap();
        let out = agent.run("where is x?".to_string(), ()).await.unwrap();

        assert_eq!(out.output, "found it");
    }

    #[test]
    fn model_errors_propagate() {
        struct Failing;
        impl ModelFactory for Failing {
            fn model_for(&self, _r: Role, _s: &str) -> Result<Arc<dyn Model>, ModelError> {
                Err(ModelError::configuration("no such model"))
            }
        }

        let reg = AgentRegistry::with_factory(OrchestratorConfig::new("."), Arc::new(Failing));

        assert!(reg.build(Role::Code).is_err());
    }
}
