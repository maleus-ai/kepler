use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, error, info};

use crate::config::{GlobalHooks, HookCommand, ServiceHooks};
use crate::errors::{DaemonError, Result};
use crate::logs::{LogStream, SharedLogBuffer};

/// Types of global hooks
#[derive(Debug, Clone, Copy)]
pub enum GlobalHookType {
    OnInit,
    OnStart,
    OnStop,
    OnCleanup,
}

impl GlobalHookType {
    pub fn as_str(&self) -> &'static str {
        match self {
            GlobalHookType::OnInit => "on_init",
            GlobalHookType::OnStart => "on_start",
            GlobalHookType::OnStop => "on_stop",
            GlobalHookType::OnCleanup => "on_cleanup",
        }
    }
}

/// Types of service hooks
#[derive(Debug, Clone, Copy)]
pub enum ServiceHookType {
    OnInit,
    OnStart,
    OnStop,
    OnRestart,
    OnExit,
}

impl ServiceHookType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceHookType::OnInit => "on_init",
            ServiceHookType::OnStart => "on_start",
            ServiceHookType::OnStop => "on_stop",
            ServiceHookType::OnRestart => "on_restart",
            ServiceHookType::OnExit => "on_exit",
        }
    }
}

/// Execute a hook command
pub async fn run_hook(
    hook: &HookCommand,
    working_dir: &Path,
    env: &HashMap<String, String>,
    logs: Option<&SharedLogBuffer>,
    service_name: &str,
) -> Result<()> {
    let (program, args) = match hook {
        HookCommand::Script { run } => {
            // Run script through shell
            ("sh".to_string(), vec!["-c".to_string(), run.clone()])
        }
        HookCommand::Command { command } => {
            if command.is_empty() {
                return Ok(());
            }
            (command[0].clone(), command[1..].to_vec())
        }
    };

    debug!("Running hook: {} {:?}", program, args);

    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(env);

    let mut child = cmd.spawn().map_err(|e| DaemonError::HookFailed {
        hook_type: format!("{} {:?}", program, args),
        message: e.to_string(),
    })?;

    // Capture stdout
    let stdout = child.stdout.take();
    let logs_for_stdout = logs.cloned();
    let service_name_stdout = service_name.to_string();
    let stdout_handle = tokio::spawn(async move {
        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // Log to daemon's log output
                info!(target: "hook", "[{}] {}", service_name_stdout, line);
                // Also capture in log buffer
                if let Some(ref logs) = logs_for_stdout {
                    logs.push(service_name_stdout.clone(), line, LogStream::Stdout);
                }
            }
        }
    });

    // Capture stderr
    let stderr = child.stderr.take();
    let logs_for_stderr = logs.cloned();
    let service_name_stderr = service_name.to_string();
    let stderr_handle = tokio::spawn(async move {
        if let Some(stderr) = stderr {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // Log to daemon's log output (as warning for stderr)
                info!(target: "hook", "[{}] {}", service_name_stderr, line);
                // Also capture in log buffer
                if let Some(ref logs) = logs_for_stderr {
                    logs.push(service_name_stderr.clone(), line, LogStream::Stderr);
                }
            }
        }
    });

    let status = child.wait().await.map_err(|e| DaemonError::HookFailed {
        hook_type: format!("{} {:?}", program, args),
        message: e.to_string(),
    })?;

    // Wait for output capture to complete
    let _ = stdout_handle.await;
    let _ = stderr_handle.await;

    if !status.success() {
        return Err(DaemonError::HookFailed {
            hook_type: format!("{} {:?}", program, args),
            message: format!("Exit code: {:?}", status.code()),
        });
    }

    Ok(())
}

/// The prefix used for global hook logs
pub const GLOBAL_HOOK_PREFIX: &str = "[global]";

/// Format the service name for global hook logs (e.g., "[global.on_start]")
pub fn global_hook_service_name(hook_type: GlobalHookType) -> String {
    format!("[global.{}]", hook_type.as_str())
}

/// Run a global hook if it exists
pub async fn run_global_hook(
    hooks: &Option<GlobalHooks>,
    hook_type: GlobalHookType,
    working_dir: &Path,
    env: &HashMap<String, String>,
    logs: Option<&SharedLogBuffer>,
) -> Result<()> {
    let hooks = match hooks {
        Some(h) => h,
        None => return Ok(()),
    };

    let hook = match hook_type {
        GlobalHookType::OnInit => &hooks.on_init,
        GlobalHookType::OnStart => &hooks.on_start,
        GlobalHookType::OnStop => &hooks.on_stop,
        GlobalHookType::OnCleanup => &hooks.on_cleanup,
    };

    if let Some(hook) = hook {
        info!("Running global {} hook", hook_type.as_str());
        let service_name = global_hook_service_name(hook_type);
        run_hook(hook, working_dir, env, logs, &service_name).await?;
    }

    Ok(())
}

/// Format the log name for service hooks (e.g., "[backend.on_start]")
pub fn service_hook_log_name(service_name: &str, hook_type: ServiceHookType) -> String {
    format!("[{}.{}]", service_name, hook_type.as_str())
}

/// Run a service hook if it exists
pub async fn run_service_hook(
    hooks: &Option<ServiceHooks>,
    hook_type: ServiceHookType,
    service_name: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
    logs: Option<&SharedLogBuffer>,
) -> Result<()> {
    let hooks = match hooks {
        Some(h) => h,
        None => return Ok(()),
    };

    let hook = match hook_type {
        ServiceHookType::OnInit => &hooks.on_init,
        ServiceHookType::OnStart => &hooks.on_start,
        ServiceHookType::OnStop => &hooks.on_stop,
        ServiceHookType::OnRestart => &hooks.on_restart,
        ServiceHookType::OnExit => &hooks.on_exit,
    };

    if let Some(hook) = hook {
        info!(
            "Running {} hook for service {}",
            hook_type.as_str(),
            service_name
        );
        let log_name = service_hook_log_name(service_name, hook_type);
        match run_hook(hook, working_dir, env, logs, &log_name).await {
            Ok(()) => {}
            Err(e) => {
                error!(
                    "Hook {} failed for service {}: {}",
                    hook_type.as_str(),
                    service_name,
                    e
                );
                return Err(e);
            }
        }
    }

    Ok(())
}
