use super::*;
use notify_debouncer_mini::DebouncedEvent;

/// Helper to build glob sets from pattern strings
fn make_glob_sets(patterns: &[&str]) -> (GlobSet, GlobSet) {
    let patterns: Vec<String> = patterns.iter().map(|s| s.to_string()).collect();
    FileWatcherActor::build_glob_sets(&patterns).unwrap()
}

/// Helper to create a DebouncedEvent with DebouncedEventKind::Any
fn file_event(path: &str) -> DebouncedEvent {
    DebouncedEvent {
        path: PathBuf::from(path),
        kind: DebouncedEventKind::Any,
    }
}

/// Helper to create a FileWatcherActor for testing process_file_events
fn make_actor(working_dir: &str) -> FileWatcherActor {
    let (tx, _rx) = mpsc::channel(1);
    FileWatcherActor::new(
        PathBuf::from("/tmp/kepler.yaml"),
        "test-service".to_string(),
        vec![],
        PathBuf::from(working_dir),
        tx,
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

// --- process_file_events tests ---

#[test]
fn process_events_include_only() {
    let actor = make_actor("/project");
    let (include, exclude) = make_glob_sets(&["src/**/*.ts"]);
    let events = vec![file_event("/project/src/app.ts")];

    assert!(actor.process_file_events(&events, &include, &exclude));
}

#[test]
fn process_events_no_match() {
    let actor = make_actor("/project");
    let (include, exclude) = make_glob_sets(&["src/**/*.ts"]);
    let events = vec![file_event("/project/src/style.css")];

    assert!(!actor.process_file_events(&events, &include, &exclude));
}

#[test]
fn process_events_negation_excludes_file() {
    let actor = make_actor("/project");
    let (include, exclude) = make_glob_sets(&["src/**/*.ts", "!**/*.test.ts"]);

    // Regular .ts file should trigger restart
    let events = vec![file_event("/project/src/app.ts")];
    assert!(actor.process_file_events(&events, &include, &exclude));

    // .test.ts file should NOT trigger restart
    let events = vec![file_event("/project/src/app.test.ts")];
    assert!(!actor.process_file_events(&events, &include, &exclude));
}

#[test]
fn process_events_negation_multiple_excludes() {
    let actor = make_actor("/project");
    let (include, exclude) =
        make_glob_sets(&["src/**/*.ts", "!**/*.test.ts", "!**/*.spec.ts"]);

    assert!(actor.process_file_events(
        &[file_event("/project/src/app.ts")],
        &include,
        &exclude
    ));
    assert!(!actor.process_file_events(
        &[file_event("/project/src/app.test.ts")],
        &include,
        &exclude
    ));
    assert!(!actor.process_file_events(
        &[file_event("/project/src/app.spec.ts")],
        &include,
        &exclude
    ));
}

#[test]
fn process_events_mixed_batch_excluded_file_does_not_trigger() {
    let actor = make_actor("/project");
    let (include, exclude) = make_glob_sets(&["src/**/*.ts", "!**/*.test.ts"]);

    // Batch with only excluded files should not trigger
    let events = vec![
        file_event("/project/src/foo.test.ts"),
        file_event("/project/src/bar.test.ts"),
    ];
    assert!(!actor.process_file_events(&events, &include, &exclude));

    // Batch with one included file should trigger
    let events = vec![
        file_event("/project/src/foo.test.ts"),
        file_event("/project/src/app.ts"),
    ];
    assert!(actor.process_file_events(&events, &include, &exclude));
}

#[test]
fn process_events_negation_with_subdirectory_pattern() {
    let actor = make_actor("/project");
    let (include, exclude) =
        make_glob_sets(&["src/**/*.ts", "!src/generated/**"]);

    assert!(actor.process_file_events(
        &[file_event("/project/src/app.ts")],
        &include,
        &exclude
    ));
    assert!(!actor.process_file_events(
        &[file_event("/project/src/generated/types.ts")],
        &include,
        &exclude
    ));
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
