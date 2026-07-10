//! Library surface for secondary binaries (`raven-eval`, etc.) and integration tests.

pub mod agent;
pub mod agent_driver;
mod agent_system_prompt;
pub mod plan_execution;
pub mod plan_intent;
pub mod plan_loop;
pub mod plan_md;
pub mod plan_protocol;
pub mod plan_recipes;
pub mod plan_verification;
pub mod chat_backend;
pub mod config;
pub mod eval_metrics;
pub mod eval_operator;
pub mod eval_smoke;
pub mod judge;
pub mod llm;
pub mod runtime;
pub mod sanitize;
pub mod server_probe;
pub mod session;
pub mod steering;
pub mod super_judge;
pub mod tools;
pub mod tool_xml;
