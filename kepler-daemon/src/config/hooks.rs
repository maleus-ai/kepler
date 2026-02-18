//! Hook configuration types for global and service-specific hooks

use serde::{Deserialize, Deserializer};
use std::path::{Path, PathBuf};

/// Global hooks that run at daemon lifecycle events
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalHooks {
    pub pre_start: Option<HookList>,
    pub post_start: Option<HookList>,
    pub pre_stop: Option<HookList>,
    pub post_stop: Option<HookList>,
    pub pre_restart: Option<HookList>,
    pub post_restart: Option<HookList>,
    pub pre_cleanup: Option<HookList>,
}

/// Service-specific hooks
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceHooks {
    pub pre_start: Option<HookList>,
    pub post_start: Option<HookList>,
    pub pre_stop: Option<HookList>,
    pub post_stop: Option<HookList>,
    pub pre_restart: Option<HookList>,
    pub post_restart: Option<HookList>,
    pub post_exit: Option<HookList>,
    pub pre_cleanup: Option<HookList>,
    pub post_healthcheck_success: Option<HookList>,
    pub post_healthcheck_fail: Option<HookList>,
}

/// Common fields shared by both hook command variants
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookCommon {
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub env_file: Option<PathBuf>,
    /// Runtime Lua condition. When present, the hook is only executed if this
    /// expression evaluates to a truthy value at runtime.
    #[serde(default, rename = "if")]
    pub condition: Option<String>,
    /// Output capture name. When set, `::output::KEY=VALUE` lines from stdout
    /// are captured and made available as `ctx.hooks.<hook_name>.<output_name>.<key>`.
    #[serde(default)]
    pub output: Option<String>,
}

/// Hook command - either a script or a command array
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum HookCommand {
    Script {
        run: String,
        #[serde(flatten)]
        common: HookCommon,
    },
    Command {
        command: Vec<String>,
        #[serde(flatten)]
        common: HookCommon,
    },
}

/// A list of hook commands that deserializes from a single object or an array.
///
/// Supports:
/// - Single hook: `pre_start: { run: "echo hi" }`
/// - Array of hooks: `post_exit: [{ run: "echo a" }, { run: "echo b", if: "..." }]`
/// - null/missing: empty list
#[derive(Debug, Clone, serde::Serialize)]
#[derive(Default)]
pub struct HookList(pub Vec<HookCommand>);


impl<'de> Deserialize<'de> for HookList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de;

        struct HookListVisitor;

        impl<'de> de::Visitor<'de> for HookListVisitor {
            type Value = HookList;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a hook command object, an array of hook commands, or null")
            }

            fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let hooks: Vec<HookCommand> =
                    Deserialize::deserialize(de::value::SeqAccessDeserializer::new(seq))?;
                Ok(HookList(hooks))
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                let hook: HookCommand =
                    Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(HookList(vec![hook]))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(HookList(Vec::new()))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(HookList(Vec::new()))
            }
        }

        deserializer.deserialize_any(HookListVisitor)
    }
}

impl HookCommand {
    /// Create a simple script hook with just a run command.
    pub fn script(run: impl Into<String>) -> Self {
        HookCommand::Script {
            run: run.into(),
            common: HookCommon::default(),
        }
    }

    pub fn common(&self) -> &HookCommon {
        match self {
            HookCommand::Script { common, .. } => common,
            HookCommand::Command { common, .. } => common,
        }
    }

    pub fn common_mut(&mut self) -> &mut HookCommon {
        match self {
            HookCommand::Script { common, .. } => common,
            HookCommand::Command { common, .. } => common,
        }
    }

    pub fn user(&self) -> Option<&str> {
        self.common().user.as_deref()
    }

    pub fn groups(&self) -> &[String] {
        &self.common().groups
    }

    pub fn working_dir(&self) -> Option<&Path> {
        self.common().working_dir.as_deref()
    }

    pub fn environment(&self) -> &[String] {
        &self.common().environment
    }

    pub fn env_file(&self) -> Option<&Path> {
        self.common().env_file.as_deref()
    }

    pub fn condition(&self) -> Option<&str> {
        self.common().condition.as_deref()
    }

    pub fn output(&self) -> Option<&str> {
        self.common().output.as_deref()
    }
}

impl GlobalHooks {
    /// Iterate over all hook slots mutably.
    pub fn all_hooks_mut(&mut self) -> impl Iterator<Item = &mut Option<HookList>> {
        [
            &mut self.pre_start,
            &mut self.post_start,
            &mut self.pre_stop,
            &mut self.post_stop,
            &mut self.pre_restart,
            &mut self.post_restart,
            &mut self.pre_cleanup,
        ]
        .into_iter()
    }
}

impl ServiceHooks {
    /// Iterate over all hook slots mutably.
    pub fn all_hooks_mut(&mut self) -> impl Iterator<Item = &mut Option<HookList>> {
        [
            &mut self.pre_start,
            &mut self.post_start,
            &mut self.pre_stop,
            &mut self.post_stop,
            &mut self.pre_restart,
            &mut self.post_restart,
            &mut self.post_exit,
            &mut self.pre_cleanup,
            &mut self.post_healthcheck_success,
            &mut self.post_healthcheck_fail,
        ]
        .into_iter()
    }
}
