use psst_application::{
    ActivationFuture, ActivationHost, ActivationPhase, ActivationPolicy, ActivationRuntime,
    ActivationSource, ActivationTurn, HarnessAdapterKind, HarnessStatusPublisher, HostFailure,
    ObservationFailure, ProfilePaths, WakeMetadata, harness_status_path, load_harness_status,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const ABRUPT_STATUS_ROOT: &str = "PSST_TEST_ABRUPT_STATUS_ROOT";

struct EmptySource;

impl ActivationSource for EmptySource {
    fn observe(
        &self,
        _maximum_wait: Duration,
    ) -> ActivationFuture<'_, Result<Option<WakeMetadata>, ObservationFailure>> {
        Box::pin(std::future::pending())
    }
}

struct UnusedHost;

impl ActivationHost for UnusedHost {
    fn start<'a>(
        &'a self,
        _wake: &'a WakeMetadata,
    ) -> ActivationFuture<'a, Result<Box<dyn ActivationTurn>, HostFailure>> {
        Box::pin(async { Err(HostFailure::Permanent) })
    }
}

fn test_paths(root: &Path) -> ProfilePaths {
    ProfilePaths {
        metadata: root.join("profiles/alpha.json"),
        credential: root.join("credentials/alpha.json"),
        lock: root.join("locks/alpha.lock"),
    }
}

fn activation() -> Arc<ActivationRuntime> {
    Arc::new(
        ActivationRuntime::start(
            Arc::new(EmptySource),
            Arc::new(UnusedHost),
            ActivationPolicy::default(),
        )
        .unwrap(),
    )
}

#[tokio::test]
#[ignore = "subprocess crash fixture"]
async fn abrupt_publisher_child() {
    let root = PathBuf::from(std::env::var_os(ABRUPT_STATUS_ROOT).unwrap());
    let _publisher = HarnessStatusPublisher::start(
        activation(),
        &test_paths(&root),
        "alpha".into(),
        HarnessAdapterKind::CodexAppServer,
    )
    .await
    .unwrap();
    std::process::abort();
}

#[tokio::test]
async fn abrupt_process_status_is_overwritten_by_restart_and_clean_stop() {
    let directory = tempfile::tempdir().unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "abrupt_publisher_child"])
        .env(ABRUPT_STATUS_ROOT, directory.path())
        .status()
        .unwrap();
    assert!(!child.success());

    let paths = test_paths(directory.path());
    let status_path = harness_status_path(&paths).unwrap();
    let abandoned = load_harness_status(&status_path).unwrap().unwrap();
    assert_eq!(abandoned.phase(), ActivationPhase::Quiet);
    assert_ne!(abandoned.owner_pid(), std::process::id());

    let activation = activation();
    let publisher = HarnessStatusPublisher::start(
        Arc::clone(&activation),
        &paths,
        "alpha".into(),
        HarnessAdapterKind::CodexAppServer,
    )
    .await
    .unwrap();
    let restarted = load_harness_status(&status_path).unwrap().unwrap();
    assert_eq!(restarted.owner_pid(), std::process::id());
    assert_eq!(restarted.phase(), ActivationPhase::Quiet);

    activation.shutdown().await;
    publisher.shutdown().await.unwrap();
    let stopped = load_harness_status(&status_path).unwrap().unwrap();
    assert_eq!(stopped.phase(), ActivationPhase::Stopped);
    assert_eq!(stopped.owner_pid(), std::process::id());
}
