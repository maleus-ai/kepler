//! Lifecycle event types for service orchestration

/// Lifecycle events that trigger different hook/retention combinations
#[derive(Debug, Clone, Copy)]
pub enum LifecycleEvent {
    /// First start
    Init,
    /// Normal start - pre_start/post_start hooks, on_start retention
    Start,
    /// Normal stop - pre_stop/post_stop hooks, on_stop retention
    Stop,
    /// Restart - pre_restart/post_restart hooks, on_restart retention
    Restart,
    /// Process exit - post_exit hook, on_exit retention + auto-restart logic
    Exit,
    /// Process exited with code 0 - on_success retention
    ExitSuccess,
    /// Process exited with non-zero code (or signal) - on_failure retention
    ExitFailure,
    /// Service skipped due to `if:` condition - on_skipped retention
    Skipped,
}
