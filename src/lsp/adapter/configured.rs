//! A server named in `structural.toml`: either a wholly generic command with
//! its languages, or an override of a built-in's command or backstop.

use super::{Adapter, Language, ReadinessPolicy};
use serde_json::Value;
use std::time::Duration;

pub struct Configured {
    pub name: String,
    pub command: Vec<String>,
    pub languages: Vec<Language>,
    pub backstop: Duration,
    /// The built-in this entry overrides, when its name matches one: the
    /// server-specific knowledge (policy, capabilities, query rewriting)
    /// still comes from there. `None` for a wholly user-defined server.
    pub base: Option<Box<dyn Adapter>>,
}

impl Adapter for Configured {
    fn name(&self) -> &str {
        &self.name
    }

    fn command(&self) -> Vec<String> {
        self.command.clone()
    }

    fn install_hint(&self) -> String {
        match &self.base {
            Some(base) => base.install_hint(),
            None => format!(
                "`{}` is configured in structural.toml; install it and ensure it is on PATH",
                self.command[0]
            ),
        }
    }

    fn languages(&self) -> Vec<Language> {
        self.languages.clone()
    }

    fn initialization_options(&self) -> Value {
        match &self.base {
            Some(base) => base.initialization_options(),
            None => serde_json::json!({}),
        }
    }

    fn experimental_capabilities(&self) -> Value {
        match &self.base {
            Some(base) => base.experimental_capabilities(),
            None => Value::Null,
        }
    }

    fn position_encodings(&self) -> Vec<&'static str> {
        match &self.base {
            Some(base) => base.position_encodings(),
            None => vec!["utf-16"],
        }
    }

    fn client_capabilities(&self) -> Value {
        match &self.base {
            Some(base) => base.client_capabilities(),
            None => super::standard_capabilities(&self.position_encodings(), Value::Null),
        }
    }

    fn readiness_policy(&self) -> Box<dyn ReadinessPolicy> {
        match &self.base {
            Some(base) => base.readiness_policy(),
            None => Box::new(super::StandardIdle),
        }
    }

    fn symbol_query(&self, query: &str) -> String {
        match &self.base {
            Some(base) => base.symbol_query(query),
            None => query.to_string(),
        }
    }

    fn backstop(&self) -> Duration {
        self.backstop
    }
}
