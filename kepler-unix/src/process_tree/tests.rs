use super::*;

/// A shell and everything it forks share its process group, so the sweep that
/// reclaims a service's escaped children has something to find.
#[test]
fn process_group_members_lists_the_leader_and_its_forks() {
    use std::os::unix::process::CommandExt;

    // `wait` keeps the shell alive, which also stops it from exec'ing into its
    // last command and collapsing the group down to one process.
    let mut leader = std::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 30 & sleep 30 & wait")
        .process_group(0)
        .spawn()
        .expect("spawn test process group");
    let leader_pid = leader.id();

    // The shell needs a moment to fork.
    let mut members = Vec::new();
    for _ in 0..60 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        members = process_group_members(leader_pid);
        if members.len() >= 3 {
            break;
        }
    }

    let _ = force_kill_process_tree(leader_pid);
    let _ = leader.wait();

    assert!(
        members.contains(&leader_pid),
        "leader missing from {:?}",
        members
    );
    assert!(
        members.len() >= 3,
        "expected the leader and both sleeps, got {:?}",
        members
    );
}

/// The two functions must agree, since callers mix them: one enumerates a group,
/// the other classifies a PID already in hand.
#[test]
fn every_member_of_a_group_reports_that_group() {
    let own_group = process_group_id(std::process::id()).expect("own process group");

    let members = process_group_members(own_group);
    assert!(
        members.contains(&std::process::id()),
        "the test process should be in its own group, got {:?}",
        members
    );
    for member in members {
        assert_eq!(process_group_id(member), Some(own_group));
    }
}
