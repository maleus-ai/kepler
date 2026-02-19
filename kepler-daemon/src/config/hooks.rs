//! Hook configuration types for global and service-specific hooks

use serde::{Deserialize, Deserializer};
use std::path::{Path, PathBuf};

use super::ConfigValue;
use super::resources::ResourceLimits;

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

/// Common fields shared by both hook command variants.
///
/// Fields are wrapped in `ConfigValue<T>` to support `${{ }}$` and `!lua` expressions,
/// just like service config fields. The `output` field is not wrapped since it's just
/// a step name label (not evaluated).
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookCommon {
    #[serde(default)]
    pub user: ConfigValue<Option<String>>,
    #[serde(default)]
    pub groups: ConfigValue<Vec<ConfigValue<String>>>,
    #[serde(default)]
    pub working_dir: ConfigValue<Option<PathBuf>>,
    #[serde(default)]
    pub environment: ConfigValue<Vec<ConfigValue<String>>>,
    #[serde(default)]
    pub env_file: ConfigValue<Option<PathBuf>>,
    /// Runtime Lua condition. When present, the hook is only executed if this
    /// expression evaluates to a truthy value at runtime.
    #[serde(default, rename = "if")]
    pub condition: ConfigValue<Option<String>>,
    /// Output capture name. When set, `::output::KEY=VALUE` lines from stdout
    /// are captured and made available as `ctx.hooks.<hook_name>.<output_name>.<key>`.
    #[serde(default)]
    pub output: Option<String>,
    /// Resource limits for the hook process
    #[serde(default)]
    pub limits: ConfigValue<Option<ResourceLimits>>,
}

/// Hook command - either a script or a command array.
///
/// The `run`/`command` fields are wrapped in `ConfigValue<T>` for dynamic evaluation.
///
/// Uses a custom Deserialize impl instead of `#[serde(untagged)]` because serde's
/// Content buffering (used by untagged enums) can't handle serde_yaml tagged values
/// like `!lua`. By capturing `serde_yaml::Value` first and dispatching on keys, we
/// avoid Content entirely.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum HookCommand {
    Script {
        run: ConfigValue<String>,
        #[serde(flatten)]
        common: HookCommon,
    },
    Command {
        command: ConfigValue<Vec<ConfigValue<String>>>,
        #[serde(flatten)]
        common: HookCommon,
    },
}

impl<'de> Deserialize<'de> for HookCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut value = serde_yaml::Value::deserialize(deserializer)
            .map_err(serde::de::Error::custom)?;
        let mapping = value.as_mapping_mut().ok_or_else(|| {
            serde::de::Error::custom("expected a mapping for hook command")
        })?;

        let run_key = serde_yaml::Value::String("run".to_string());
        let cmd_key = serde_yaml::Value::String("command".to_string());

        if let Some(run_value) = mapping.remove(&run_key) {
            // Script variant: extract `run`, deserialize rest as HookCommon
            let run: ConfigValue<String> = serde_yaml::from_value(run_value)
                .map_err(serde::de::Error::custom)?;
            let common: HookCommon = serde_yaml::from_value(value)
                .map_err(serde::de::Error::custom)?;
            Ok(HookCommand::Script { run, common })
        } else if let Some(cmd_value) = mapping.remove(&cmd_key) {
            // Command variant: extract `command`, deserialize rest as HookCommon
            let command: ConfigValue<Vec<ConfigValue<String>>> =
                serde_yaml::from_value(cmd_value).map_err(serde::de::Error::custom)?;
            let common: HookCommon = serde_yaml::from_value(value)
                .map_err(serde::de::Error::custom)?;
            Ok(HookCommand::Command { command, common })
        } else {
            Err(serde::de::Error::custom(
                "hook command must have either 'run' or 'command' key",
            ))
        }
    }
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
            run: run.into().into(),
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

    /// Get the user field (static value only).
    pub fn user(&self) -> Option<&str> {
        self.common().user.as_static().and_then(|v| v.as_deref())
    }

    /// Get groups (static value only).
    pub fn groups(&self) -> Vec<String> {
        match self.common().groups.as_static() {
            Some(items) => items.iter().filter_map(|v| v.as_static().cloned()).collect(),
            None => Vec::new(),
        }
    }

    /// Get working_dir (static value only).
    pub fn working_dir(&self) -> Option<&Path> {
        self.common().working_dir.as_static().and_then(|v| v.as_deref())
    }

    /// Get environment entries (static value only).
    pub fn environment(&self) -> Vec<String> {
        match self.common().environment.as_static() {
            Some(items) => items.iter().filter_map(|v| v.as_static().cloned()).collect(),
            None => Vec::new(),
        }
    }

    /// Get env_file path (static value only).
    pub fn env_file(&self) -> Option<&Path> {
        self.common().env_file.as_static().and_then(|v| v.as_deref())
    }

    /// Get condition string (static value only).
    pub fn condition(&self) -> Option<&str> {
        self.common().condition.as_static().and_then(|v| v.as_deref())
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
