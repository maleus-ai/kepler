use super::*;
use notify::event::{
    AccessKind, AccessMode, CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind,
    RenameMode,
};

// --- is_content_modifying tests ---

#[test]
fn access_open_read_is_not_content_modifying() {
    assert!(!is_content_modifying(&EventKind::Access(AccessKind::Open(
        AccessMode::Read
    ))));
}

#[test]
fn access_open_write_is_not_content_modifying() {
    assert!(!is_content_modifying(&EventKind::Access(AccessKind::Open(
        AccessMode::Write
    ))));
}

#[test]
fn access_open_any_is_not_content_modifying() {
    assert!(!is_content_modifying(&EventKind::Access(AccessKind::Open(
        AccessMode::Any
    ))));
}

#[test]
fn access_close_read_is_not_content_modifying() {
    assert!(!is_content_modifying(&EventKind::Access(
        AccessKind::Close(AccessMode::Read)
    )));
}

#[test]
fn access_close_write_is_not_content_modifying() {
    assert!(!is_content_modifying(&EventKind::Access(
        AccessKind::Close(AccessMode::Write)
    )));
}

#[test]
fn access_read_is_not_content_modifying() {
    assert!(!is_content_modifying(&EventKind::Access(
        AccessKind::Read
    )));
}

#[test]
fn access_any_is_not_content_modifying() {
    assert!(!is_content_modifying(&EventKind::Access(
        AccessKind::Any
    )));
}

#[test]
fn create_file_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Create(CreateKind::File)));
}

#[test]
fn create_folder_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Create(
        CreateKind::Folder
    )));
}

#[test]
fn create_any_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Create(CreateKind::Any)));
}

#[test]
fn modify_data_content_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Modify(
        ModifyKind::Data(DataChange::Content)
    )));
}

#[test]
fn modify_data_size_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Modify(
        ModifyKind::Data(DataChange::Size)
    )));
}

#[test]
fn modify_data_any_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Modify(
        ModifyKind::Data(DataChange::Any)
    )));
}

#[test]
fn modify_metadata_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Modify(
        ModifyKind::Metadata(MetadataKind::Any)
    )));
}

#[test]
fn modify_name_rename_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Modify(
        ModifyKind::Name(RenameMode::Both)
    )));
}

#[test]
fn modify_any_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Modify(
        ModifyKind::Any
    )));
}

#[test]
fn remove_file_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Remove(
        RemoveKind::File
    )));
}

#[test]
fn remove_folder_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Remove(
        RemoveKind::Folder
    )));
}

#[test]
fn remove_any_is_content_modifying() {
    assert!(is_content_modifying(&EventKind::Remove(RemoveKind::Any)));
}

#[test]
fn event_kind_any_is_not_content_modifying() {
    assert!(!is_content_modifying(&EventKind::Any));
}

#[test]
fn event_kind_other_is_not_content_modifying() {
    assert!(!is_content_modifying(&EventKind::Other));
}

// --- extract_base_dir tests ---

#[test]
fn extract_base_dir_simple_glob() {
    assert_eq!(
        extract_base_dir("/home/user/src/**/*.ts"),
        Some(PathBuf::from("/home/user/src"))
    );
}

#[test]
fn extract_base_dir_glob_at_start() {
    assert_eq!(extract_base_dir("*.ts"), None);
}

#[test]
fn extract_base_dir_no_glob() {
    assert_eq!(
        extract_base_dir("/home/user/src/app.ts"),
        Some(PathBuf::from("/home/user/src"))
    );
}

#[test]
fn extract_base_dir_multibyte_utf8() {
    // Must not panic — previously used char index instead of byte index
    assert_eq!(
        extract_base_dir("日本語/*.ts"),
        Some(PathBuf::from("日本語"))
    );
}

#[test]
fn extract_base_dir_multibyte_utf8_absolute() {
    assert_eq!(
        extract_base_dir("/données/café/*.rs"),
        Some(PathBuf::from("/données/café"))
    );
}

/// Helper to build glob sets from pattern strings
fn make_glob_sets(patterns: &[&str]) -> (GlobSet, GlobSet) {
    let patterns: Vec<String> = patterns.iter().map(|s| s.to_string()).collect();
    FileWatcherActor::build_glob_sets(&patterns).unwrap()
}

/// Helper to create a FileWatcherActor for testing process_file_paths
fn make_actor(working_dir: &str) -> FileWatcherActor {
    let (tx, _rx) = mpsc::channel(1);
    FileWatcherActor::new(
        PathBuf::from("/tmp/kepler.yaml"),
        "test-service".to_string(),
        vec![],
        PathBuf::from(working_dir),
        tx,
        Arc::new(AtomicBool::new(false)),
    )
}

// --- build_glob_sets tests ---

#[test]
fn build_glob_sets_empty_returns_none() {
    let patterns: Vec<String> = vec![];
    assert!(FileWatcherActor::build_glob_sets(&patterns).is_none());
}

#[test]
fn build_glob_sets_only_excludes_returns_none() {
    let patterns = vec!["!**/*.test.ts".to_string()];
    assert!(FileWatcherActor::build_glob_sets(&patterns).is_none());
}

#[test]
fn build_glob_sets_splits_include_and_exclude() {
    let (include, exclude) = make_glob_sets(&["**/*.ts", "!**/*.test.ts"]);

    // Include matches .ts files
    assert!(include.is_match("src/app.ts"));
    assert!(include.is_match("src/app.test.ts"));

    // Exclude only matches .test.ts files
    assert!(!exclude.is_match("src/app.ts"));
    assert!(exclude.is_match("src/app.test.ts"));
}

#[test]
fn build_glob_sets_multiple_excludes() {
    let (include, exclude) =
        make_glob_sets(&["src/**/*.ts", "!**/*.test.ts", "!**/*.spec.ts"]);

    assert!(include.is_match("src/app.ts"));
    assert!(exclude.is_match("src/app.test.ts"));
    assert!(exclude.is_match("src/app.spec.ts"));
    assert!(!exclude.is_match("src/app.ts"));
}

// --- process_file_paths tests ---

#[test]
fn process_paths_include_only() {
    let actor = make_actor("/project");
    let (include, exclude) = make_glob_sets(&["src/**/*.ts"]);
    let paths = vec![PathBuf::from("/project/src/app.ts")];

    assert!(!actor.process_file_paths(&paths, &include, &exclude).is_empty());
}

#[test]
fn process_paths_no_match() {
    let actor = make_actor("/project");
    let (include, exclude) = make_glob_sets(&["src/**/*.ts"]);
    let paths = vec![PathBuf::from("/project/src/style.css")];

    assert!(actor.process_file_paths(&paths, &include, &exclude).is_empty());
}

#[test]
fn process_paths_negation_excludes_file() {
    let actor = make_actor("/project");
    let (include, exclude) = make_glob_sets(&["src/**/*.ts", "!**/*.test.ts"]);

    // Regular .ts file should trigger restart
    let paths = vec![PathBuf::from("/project/src/app.ts")];
    assert!(!actor.process_file_paths(&paths, &include, &exclude).is_empty());

    // .test.ts file should NOT trigger restart
    let paths = vec![PathBuf::from("/project/src/app.test.ts")];
    assert!(actor.process_file_paths(&paths, &include, &exclude).is_empty());
}

#[test]
fn process_paths_negation_multiple_excludes() {
    let actor = make_actor("/project");
    let (include, exclude) =
        make_glob_sets(&["src/**/*.ts", "!**/*.test.ts", "!**/*.spec.ts"]);

    assert!(!actor.process_file_paths(
        &[PathBuf::from("/project/src/app.ts")],
        &include,
        &exclude
    ).is_empty());
    assert!(actor.process_file_paths(
        &[PathBuf::from("/project/src/app.test.ts")],
        &include,
        &exclude
    ).is_empty());
    assert!(actor.process_file_paths(
        &[PathBuf::from("/project/src/app.spec.ts")],
        &include,
        &exclude
    ).is_empty());
}

#[test]
fn process_paths_mixed_batch_excluded_file_does_not_trigger() {
    let actor = make_actor("/project");
    let (include, exclude) = make_glob_sets(&["src/**/*.ts", "!**/*.test.ts"]);

    // Batch with only excluded files should not trigger
    let paths = vec![
        PathBuf::from("/project/src/foo.test.ts"),
        PathBuf::from("/project/src/bar.test.ts"),
    ];
    assert!(actor.process_file_paths(&paths, &include, &exclude).is_empty());

    // Batch with one included file should trigger
    let paths = vec![
        PathBuf::from("/project/src/foo.test.ts"),
        PathBuf::from("/project/src/app.ts"),
    ];
    assert!(!actor.process_file_paths(&paths, &include, &exclude).is_empty());
}

#[test]
fn process_paths_negation_with_subdirectory_pattern() {
    let actor = make_actor("/project");
    let (include, exclude) =
        make_glob_sets(&["src/**/*.ts", "!src/generated/**"]);

    assert!(!actor.process_file_paths(
        &[PathBuf::from("/project/src/app.ts")],
        &include,
        &exclude
    ).is_empty());
    assert!(actor.process_file_paths(
        &[PathBuf::from("/project/src/generated/types.ts")],
        &include,
        &exclude
    ).is_empty());
}

// --- process_file_paths: matched_files content tests ---

#[test]
fn process_paths_returns_correct_matched_path() {
    let actor = make_actor("/project");
    let (include, exclude) = make_glob_sets(&["src/**/*.ts"]);
    let paths = vec![PathBuf::from("/project/src/app.ts")];

    let matched = actor.process_file_paths(&paths, &include, &exclude);
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0], PathBuf::from("/project/src/app.ts"));
}

#[test]
fn process_paths_batch_returns_all_matched_files() {
    let actor = make_actor("/project");
    let (include, exclude) = make_glob_sets(&["src/**/*.ts"]);
    let paths = vec![
        PathBuf::from("/project/src/app.ts"),
        PathBuf::from("/project/src/utils.ts"),
        PathBuf::from("/project/src/index.ts"),
    ];

    let matched = actor.process_file_paths(&paths, &include, &exclude);
    assert_eq!(matched.len(), 3);
    assert!(matched.contains(&PathBuf::from("/project/src/app.ts")));
    assert!(matched.contains(&PathBuf::from("/project/src/utils.ts")));
    assert!(matched.contains(&PathBuf::from("/project/src/index.ts")));
}

#[test]
fn process_paths_batch_only_returns_matching_files() {
    let actor = make_actor("/project");
    let (include, exclude) = make_glob_sets(&["src/**/*.ts", "!**/*.test.ts"]);
    let paths = vec![
        PathBuf::from("/project/src/app.ts"),
        PathBuf::from("/project/src/app.test.ts"),
        PathBuf::from("/project/src/style.css"),
        PathBuf::from("/project/src/utils.ts"),
    ];

    let matched = actor.process_file_paths(&paths, &include, &exclude);
    assert_eq!(matched.len(), 2);
    assert!(matched.contains(&PathBuf::from("/project/src/app.ts")));
    assert!(matched.contains(&PathBuf::from("/project/src/utils.ts")));
}

// --- collect_watch_dirs tests ---

#[test]
fn collect_watch_dirs_skips_negation_patterns() {
    let patterns = vec![
        "src/**/*.ts".to_string(),
        "!**/*.test.ts".to_string(),
    ];
    let dirs = FileWatcherActor::collect_watch_dirs(&patterns, Path::new("/project"));
    // Only the include pattern contributes the default dir
    assert_eq!(dirs.len(), 1);
    assert!(dirs.contains(&PathBuf::from("/project")));
}

// --- suppression tests ---

#[tokio::test]
async fn suppressed_watcher_discards_events() {
    let suppressed = Arc::new(AtomicBool::new(false));
    let (restart_tx, mut restart_rx) = mpsc::channel::<FileChangeEvent>(16);
    let (event_tx, mut event_rx) = mpsc::channel::<Vec<PathBuf>>(32);

    let flag = suppressed.clone();
    let send_tx = restart_tx.clone();

    // Simulate the actor's event loop inline
    let actor_task = tokio::spawn(async move {
        let include_patterns = vec!["src/**/*.ts".to_string()];
        let (include_set, exclude_set) =
            FileWatcherActor::build_glob_sets(&include_patterns).unwrap();

        let actor = FileWatcherActor::new(
            PathBuf::from("/tmp/test.yaml"),
            "test".to_string(),
            include_patterns,
            PathBuf::from("/project"),
            restart_tx,
            flag.clone(),
        );

        let mut was_suppressed = false;
        while let Some(paths) = event_rx.recv().await {
            let currently_suppressed = flag.load(Ordering::Acquire);
            if was_suppressed && !currently_suppressed {
                while event_rx.try_recv().is_ok() {}
                was_suppressed = false;
                continue;
            }
            was_suppressed = currently_suppressed;
            if currently_suppressed {
                continue;
            }
            let matched = actor.process_file_paths(&paths, &include_set, &exclude_set);
            if !matched.is_empty() {
                let evt = FileChangeEvent {
                    config_path: PathBuf::from("/tmp/test.yaml"),
                    service_name: "test".to_string(),
                    matched_files: matched,
                };
                if send_tx.send(evt).await.is_err() {
                    break;
                }
            }
        }
    });

    // Send an event while NOT suppressed — should be forwarded
    event_tx
        .send(vec![PathBuf::from("/project/src/app.ts")])
        .await
        .unwrap();
    let evt = tokio::time::timeout(std::time::Duration::from_secs(1), restart_rx.recv()).await;
    assert!(evt.is_ok(), "Expected event to be forwarded when not suppressed");

    // Suppress the watcher
    suppressed.store(true, Ordering::Release);

    // Send events while suppressed — should NOT be forwarded
    event_tx
        .send(vec![PathBuf::from("/project/src/suppressed.ts")])
        .await
        .unwrap();
    let evt = tokio::time::timeout(std::time::Duration::from_millis(100), restart_rx.recv()).await;
    assert!(evt.is_err(), "Expected no event while suppressed");

    // Clean up
    actor_task.abort();
}

#[tokio::test]
async fn resume_drains_stale_events() {
    let suppressed = Arc::new(AtomicBool::new(true)); // start suppressed
    let (restart_tx, mut restart_rx) = mpsc::channel::<FileChangeEvent>(16);
    let (event_tx, mut event_rx) = mpsc::channel::<Vec<PathBuf>>(32);

    let flag = suppressed.clone();
    let send_tx = restart_tx.clone();

    let actor_task = tokio::spawn(async move {
        let include_patterns = vec!["src/**/*.ts".to_string()];
        let (include_set, exclude_set) =
            FileWatcherActor::build_glob_sets(&include_patterns).unwrap();

        let actor = FileWatcherActor::new(
            PathBuf::from("/tmp/test.yaml"),
            "test".to_string(),
            include_patterns,
            PathBuf::from("/project"),
            restart_tx,
            flag.clone(),
        );

        let mut was_suppressed = false;
        while let Some(paths) = event_rx.recv().await {
            let currently_suppressed = flag.load(Ordering::Acquire);
            if was_suppressed && !currently_suppressed {
                while event_rx.try_recv().is_ok() {}
                was_suppressed = false;
                continue;
            }
            was_suppressed = currently_suppressed;
            if currently_suppressed {
                continue;
            }
            let matched = actor.process_file_paths(&paths, &include_set, &exclude_set);
            if !matched.is_empty() {
                let evt = FileChangeEvent {
                    config_path: PathBuf::from("/tmp/test.yaml"),
                    service_name: "test".to_string(),
                    matched_files: matched,
                };
                if send_tx.send(evt).await.is_err() {
                    break;
                }
            }
        }
    });

    // Queue stale events while suppressed
    event_tx
        .send(vec![PathBuf::from("/project/src/stale1.ts")])
        .await
        .unwrap();
    event_tx
        .send(vec![PathBuf::from("/project/src/stale2.ts")])
        .await
        .unwrap();

    // Give the actor time to process (discard) the first event
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Resume the watcher — next event should trigger drain of stale2,
    // and subsequent events should be processed normally.
    suppressed.store(false, Ordering::Release);

    // Send a fresh event after resume — this triggers the drain transition
    // (stale2 is still in the channel and will be drained).
    // We need a "trigger" event to wake the loop after the suppressed ones.
    event_tx
        .send(vec![PathBuf::from("/project/src/trigger.ts")])
        .await
        .unwrap();

    // Give the actor time to process the trigger (which does the drain + continue)
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Now send a real event that should be processed normally
    event_tx
        .send(vec![PathBuf::from("/project/src/fresh.ts")])
        .await
        .unwrap();

    let evt = tokio::time::timeout(std::time::Duration::from_secs(1), restart_rx.recv()).await;
    assert!(evt.is_ok(), "Expected fresh event to be forwarded after resume");

    // Should be no more stale events
    let extra = tokio::time::timeout(std::time::Duration::from_millis(100), restart_rx.recv()).await;
    assert!(extra.is_err(), "Expected no stale events after drain");

    actor_task.abort();
}

// --- FileWatcherHandle tests ---

#[tokio::test]
async fn file_watcher_handle_matches_same_patterns() {
    let handle = FileWatcherHandle {
        task_handle: tokio::spawn(async {}),
        suppressed: Arc::new(AtomicBool::new(false)),
        patterns: vec!["src/**/*.ts".to_string()],
        working_dir: PathBuf::from("/project"),
    };

    assert!(handle.matches(
        &["src/**/*.ts".to_string()],
        Path::new("/project"),
    ));
}

#[tokio::test]
async fn file_watcher_handle_no_match_different_patterns() {
    let handle = FileWatcherHandle {
        task_handle: tokio::spawn(async {}),
        suppressed: Arc::new(AtomicBool::new(false)),
        patterns: vec!["src/**/*.ts".to_string()],
        working_dir: PathBuf::from("/project"),
    };

    assert!(!handle.matches(
        &["src/**/*.rs".to_string()],
        Path::new("/project"),
    ));
}

#[tokio::test]
async fn file_watcher_handle_no_match_different_dir() {
    let handle = FileWatcherHandle {
        task_handle: tokio::spawn(async {}),
        suppressed: Arc::new(AtomicBool::new(false)),
        patterns: vec!["src/**/*.ts".to_string()],
        working_dir: PathBuf::from("/project"),
    };

    assert!(!handle.matches(
        &["src/**/*.ts".to_string()],
        Path::new("/other"),
    ));
}

#[tokio::test]
async fn file_watcher_handle_suppress_and_resume() {
    let flag = Arc::new(AtomicBool::new(false));
    let handle = FileWatcherHandle {
        task_handle: tokio::spawn(async {}),
        suppressed: flag.clone(),
        patterns: vec![],
        working_dir: PathBuf::from("/project"),
    };

    assert!(!flag.load(Ordering::Acquire));
    handle.suppress();
    assert!(flag.load(Ordering::Acquire));
    handle.resume();
    assert!(!flag.load(Ordering::Acquire));
}

#[tokio::test]
async fn double_suppress_is_idempotent() {
    let flag = Arc::new(AtomicBool::new(false));
    let handle = FileWatcherHandle {
        task_handle: tokio::spawn(async {}),
        suppressed: flag.clone(),
        patterns: vec![],
        working_dir: PathBuf::from("/project"),
    };

    handle.suppress();
    assert!(flag.load(Ordering::Acquire));
    handle.suppress();
    assert!(flag.load(Ordering::Acquire));
    handle.resume();
    assert!(!flag.load(Ordering::Acquire));
}

#[tokio::test]
async fn resume_without_suppress_is_noop() {
    let flag = Arc::new(AtomicBool::new(false));
    let handle = FileWatcherHandle {
        task_handle: tokio::spawn(async {}),
        suppressed: flag.clone(),
        patterns: vec![],
        working_dir: PathBuf::from("/project"),
    };

    // Resume when not suppressed — flag stays false
    handle.resume();
    assert!(!flag.load(Ordering::Acquire));
}

#[tokio::test]
async fn finished_watcher_is_not_reusable() {
    // Spawn a task that completes immediately
    let task = tokio::spawn(async {});
    // Wait for it to finish
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let handle = FileWatcherHandle {
        task_handle: task,
        suppressed: Arc::new(AtomicBool::new(false)),
        patterns: vec!["src/**/*.ts".to_string()],
        working_dir: PathBuf::from("/project"),
    };

    // Patterns match, but watcher is finished — should not be reusable
    assert!(handle.is_finished());
    assert!(handle.matches(
        &["src/**/*.ts".to_string()],
        Path::new("/project"),
    ));
    // In the actor, ResumeFileWatcher checks both is_finished() AND matches()
    // A finished watcher should not be resumed even if patterns match
}

#[tokio::test]
async fn normal_processing_resumes_after_suppress_resume_cycle() {
    let suppressed = Arc::new(AtomicBool::new(false));
    let (restart_tx, mut restart_rx) = mpsc::channel::<FileChangeEvent>(16);
    let (event_tx, mut event_rx) = mpsc::channel::<Vec<PathBuf>>(32);

    let flag = suppressed.clone();
    let send_tx = restart_tx.clone();

    let actor_task = tokio::spawn(async move {
        let include_patterns = vec!["src/**/*.ts".to_string()];
        let (include_set, exclude_set) =
            FileWatcherActor::build_glob_sets(&include_patterns).unwrap();

        let actor = FileWatcherActor::new(
            PathBuf::from("/tmp/test.yaml"),
            "test".to_string(),
            include_patterns,
            PathBuf::from("/project"),
            restart_tx,
            flag.clone(),
        );

        let mut was_suppressed = false;
        while let Some(paths) = event_rx.recv().await {
            let currently_suppressed = flag.load(Ordering::Acquire);
            if was_suppressed && !currently_suppressed {
                while event_rx.try_recv().is_ok() {}
                was_suppressed = false;
                continue;
            }
            was_suppressed = currently_suppressed;
            if currently_suppressed {
                continue;
            }
            let matched = actor.process_file_paths(&paths, &include_set, &exclude_set);
            if !matched.is_empty() {
                let evt = FileChangeEvent {
                    config_path: PathBuf::from("/tmp/test.yaml"),
                    service_name: "test".to_string(),
                    matched_files: matched,
                };
                if send_tx.send(evt).await.is_err() {
                    break;
                }
            }
        }
    });

    // Phase 1: Normal event → forwarded
    event_tx.send(vec![PathBuf::from("/project/src/a.ts")]).await.unwrap();
    let evt = tokio::time::timeout(std::time::Duration::from_secs(1), restart_rx.recv()).await;
    assert!(evt.is_ok(), "Phase 1: event should be forwarded");

    // Phase 2: Suppress → events discarded
    suppressed.store(true, Ordering::Release);
    event_tx.send(vec![PathBuf::from("/project/src/stale.ts")]).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Phase 3: Resume → drain trigger event consumed, then normal processing resumes
    suppressed.store(false, Ordering::Release);
    // This event triggers the drain transition (gets consumed by the drain logic)
    event_tx.send(vec![PathBuf::from("/project/src/drain_trigger.ts")]).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Phase 4: Post-resume event → forwarded normally
    event_tx.send(vec![PathBuf::from("/project/src/b.ts")]).await.unwrap();
    let evt = tokio::time::timeout(std::time::Duration::from_secs(1), restart_rx.recv()).await;
    assert!(evt.is_ok(), "Phase 4: event should be forwarded after resume");

    // No stale events leaked through
    let extra = tokio::time::timeout(std::time::Duration::from_millis(100), restart_rx.recv()).await;
    assert!(extra.is_err(), "No stale events should leak");

    actor_task.abort();
}
