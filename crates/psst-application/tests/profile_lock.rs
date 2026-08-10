use psst_application::ProfileLock;
use std::{fs, path::Path, process::Command};

#[test]
fn lock_probe_child() {
    let Ok(path) = std::env::var("PSST_LOCK_PROBE") else {
        return;
    };
    let expected = std::env::var("PSST_LOCK_EXPECT").unwrap();
    assert_eq!(
        ProfileLock::acquire(Path::new(&path)).is_ok(),
        expected == "open"
    );
}

#[test]
fn replacement_cannot_create_cross_process_owner() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("locks/x.lock");
    let guard = ProfileLock::acquire(&p).unwrap();
    let _ = fs::rename(&p, t.path().join("moved.lock"));
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_probe_child"])
        .env("PSST_LOCK_PROBE", &p)
        .env("PSST_LOCK_EXPECT", "closed")
        .status()
        .unwrap();
    assert!(status.success());
    drop(guard);
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_probe_child"])
        .env("PSST_LOCK_PROBE", &p)
        .env("PSST_LOCK_EXPECT", "open")
        .status()
        .unwrap();
    assert!(status.success());
}
