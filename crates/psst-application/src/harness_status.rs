use crate::{
    ActivationPhase, ActivationSnapshot, ConfigError, MAX_BACKOFF_ATTEMPTS, MAX_WAKE_PENDING_COUNT,
    ProfilePaths, atomic_replace, open_directory_guard, reject_symlink, validate_profile_name,
};
use psst_protocol::{ApiTimestamp, MessagePriorityDto};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::fs::File;
#[cfg(windows)]
use std::fs::OpenOptions;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
    time::Instant,
};

const HARNESS_STATUS_VERSION: u32 = 1;
pub const MAX_HARNESS_STATUS_BYTES: u64 = 16 * 1024;
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(250);
const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const STATUS_STALE_AFTER: time::Duration = time::Duration::seconds(90);
const STATUS_FUTURE_SKEW: time::Duration = time::Duration::minutes(5);
const STATUS_FAILURE_DIAGNOSTIC: &str =
    "psst: harness status publication failed; activation stopped to preserve observable ownership";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessAdapterKind {
    ClaudeChannel,
    CodexAppServer,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessStatusRecord {
    version: u32,
    profile: String,
    adapter: HarnessAdapterKind,
    phase: ActivationPhase,
    retry_attempt: u8,
    pending_count: Option<u64>,
    highest_priority: Option<MessagePriorityDto>,
    owner_pid: u32,
    observed_at: ApiTimestamp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessStatusFreshness {
    Recent,
    Stale,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessStatusView {
    #[serde(flatten)]
    pub record: HarnessStatusRecord,
    pub freshness: HarnessStatusFreshness,
}

pub struct HarnessStatusPublisher {
    activation: Arc<crate::ActivationRuntime>,
    path: PathBuf,
    profile: String,
    adapter: HarnessAdapterKind,
    stop: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<io::Result<()>>>>,
}

impl std::fmt::Debug for HarnessStatusPublisher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HarnessStatusPublisher")
            .field("profile", &self.profile)
            .field("adapter", &self.adapter)
            .finish_non_exhaustive()
    }
}

impl HarnessStatusPublisher {
    /// Starts one bounded status publisher for an already-owned activation runtime.
    ///
    /// # Errors
    /// Fails before spawning when the initial status cannot be validated and durably published.
    pub async fn start(
        activation: Arc<crate::ActivationRuntime>,
        paths: &ProfilePaths,
        profile: String,
        adapter: HarnessAdapterKind,
    ) -> io::Result<Arc<Self>> {
        let path = harness_status_path(paths).map_err(invalid_data)?;
        let initial = activation.snapshot().await;
        publish_snapshot(&path, &profile, adapter, &initial)?;
        let (stop, stop_rx) = watch::channel(false);
        let publisher = Arc::new(Self {
            activation,
            path,
            profile,
            adapter,
            stop,
            task: Mutex::new(None),
        });
        let task = tokio::spawn(run_status_publisher(
            Arc::clone(&publisher),
            stop_rx,
            initial,
        ));
        *publisher.task.lock().await = Some(task);
        Ok(publisher)
    }

    /// Stops publication after the activation runtime has reached its terminal phase and writes
    /// that final snapshot durably.
    ///
    /// # Errors
    /// Propagates a background or final publication failure.
    pub async fn shutdown(&self) -> io::Result<()> {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.lock().await.take() {
            task.await
                .map_err(|_| io::Error::other("harness status task failed"))??;
        }
        publish_snapshot(
            &self.path,
            &self.profile,
            self.adapter,
            &self.activation.snapshot().await,
        )
    }
}

impl Drop for HarnessStatusPublisher {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Ok(task) = self.task.try_lock()
            && let Some(task) = task.as_ref()
        {
            task.abort();
        }
    }
}

async fn run_status_publisher(
    publisher: Arc<HarnessStatusPublisher>,
    mut stop: watch::Receiver<bool>,
    mut prior: ActivationSnapshot,
) -> io::Result<()> {
    let mut last_write = Instant::now();
    loop {
        tokio::select! {
            biased;
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return Ok(());
                }
            }
            () = tokio::time::sleep(STATUS_POLL_INTERVAL) => {
                let current = publisher.activation.snapshot().await;
                if current != prior || last_write.elapsed() >= STATUS_REFRESH_INTERVAL {
                    if let Err(error) = publish_snapshot(
                        &publisher.path,
                        &publisher.profile,
                        publisher.adapter,
                        &current,
                    ) {
                        eprintln!("{STATUS_FAILURE_DIAGNOSTIC}");
                        publisher.activation.shutdown().await;
                        return Err(error);
                    }
                    prior = current;
                    last_write = Instant::now();
                }
            }
        }
    }
}

fn publish_snapshot(
    path: &Path,
    profile: &str,
    adapter: HarnessAdapterKind,
    snapshot: &ActivationSnapshot,
) -> io::Result<()> {
    let now = time::OffsetDateTime::now_utc();
    let observed_at = ApiTimestamp::new(
        now.replace_nanosecond((now.nanosecond() / 1_000_000) * 1_000_000)
            .map_err(invalid_data)?,
    )
    .map_err(invalid_data)?;
    let record =
        HarnessStatusRecord::from_snapshot(profile.to_owned(), adapter, snapshot, observed_at)
            .map_err(invalid_data)?;
    store_harness_status(path, &record)
}

impl HarnessStatusRecord {
    /// Creates a bounded, non-secret operator snapshot from the activation machine.
    ///
    /// # Errors
    /// Rejects invalid profile identity or an internally inconsistent activation snapshot.
    pub fn from_snapshot(
        profile: String,
        adapter: HarnessAdapterKind,
        snapshot: &ActivationSnapshot,
        observed_at: ApiTimestamp,
    ) -> Result<Self, ConfigError> {
        let record = Self {
            version: HARNESS_STATUS_VERSION,
            profile,
            adapter,
            phase: snapshot.phase,
            retry_attempt: snapshot.retry_attempt,
            pending_count: snapshot
                .pending
                .as_ref()
                .map(crate::WakeMetadata::pending_count),
            highest_priority: snapshot
                .pending
                .as_ref()
                .map(crate::WakeMetadata::highest_priority),
            owner_pid: std::process::id(),
            observed_at,
        };
        record.validate()?;
        Ok(record)
    }

    /// Validates a status record regardless of whether it came from construction or disk.
    ///
    /// # Errors
    /// Rejects unknown versions, invalid identity, impossible phases, and unbounded counters.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.version != HARNESS_STATUS_VERSION {
            return Err(ConfigError::Invalid("harness status version"));
        }
        validate_profile_name(&self.profile)?;
        if self.owner_pid == 0 || self.retry_attempt > MAX_BACKOFF_ATTEMPTS {
            return Err(ConfigError::Invalid("harness status owner"));
        }
        match (self.pending_count, self.highest_priority) {
            (None, None) => {}
            (Some(count), Some(_)) if (1..=MAX_WAKE_PENDING_COUNT).contains(&count) => {}
            _ => return Err(ConfigError::Invalid("harness status pending state")),
        }
        let has_pending = self.pending_count.is_some();
        match self.phase {
            ActivationPhase::Quiet | ActivationPhase::Stopped if has_pending => {
                return Err(ConfigError::Invalid("harness status phase"));
            }
            ActivationPhase::Pending
            | ActivationPhase::Waking
            | ActivationPhase::Running
            | ActivationPhase::Backoff
                if !has_pending =>
            {
                return Err(ConfigError::Invalid("harness status phase"));
            }
            ActivationPhase::Quiet
            | ActivationPhase::Pending
            | ActivationPhase::Waking
            | ActivationPhase::Running
            | ActivationPhase::Backoff
            | ActivationPhase::Blocked
            | ActivationPhase::Stopped => {}
        }
        Ok(())
    }

    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    #[must_use]
    pub const fn adapter(&self) -> HarnessAdapterKind {
        self.adapter
    }

    #[must_use]
    pub const fn phase(&self) -> ActivationPhase {
        self.phase
    }

    #[must_use]
    pub const fn retry_attempt(&self) -> u8 {
        self.retry_attempt
    }

    #[must_use]
    pub const fn pending_count(&self) -> Option<u64> {
        self.pending_count
    }

    #[must_use]
    pub const fn owner_pid(&self) -> u32 {
        self.owner_pid
    }

    #[must_use]
    pub const fn observed_at(&self) -> ApiTimestamp {
        self.observed_at
    }
}

/// Stores a validated status snapshot with the same protected atomic boundary as profile metadata.
///
/// # Errors
/// Rejects invalid records and path substitution, and propagates durable-write errors.
pub fn store_harness_status(path: &Path, record: &HarnessStatusRecord) -> io::Result<()> {
    record.validate().map_err(invalid_data)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "status parent unavailable"))?;
    fs::create_dir_all(parent)?;
    reject_symlink(parent)?;
    let _directory_guard = open_directory_guard(parent)?;
    let bytes = serde_json::to_vec(record).map_err(invalid_data)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_HARNESS_STATUS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "harness status record too large",
        ));
    }
    atomic_replace(path, &bytes)
}

/// Loads and validates a bounded non-secret harness status snapshot.
///
/// # Errors
/// Rejects corruption, unsupported versions, oversized records, and path substitution.
pub fn load_harness_status(path: &Path) -> io::Result<Option<HarnessStatusRecord>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "harness status link substitution rejected",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "status parent unavailable"))?;
    let directory_guard = open_directory_guard(parent)?;
    #[cfg(windows)]
    let _ = &directory_guard;
    reject_symlink(path)?;
    #[cfg(windows)]
    let mut options = OpenOptions::new();
    #[cfg(windows)]
    options.read(true);
    #[cfg(windows)]
    {
        options.share_mode(1 | 2).custom_flags(0x0020_0000);
    }
    #[cfg(windows)]
    let file = options.open(path)?;
    #[cfg(unix)]
    let file = File::from(
        rustix::fs::openat(
            &directory_guard,
            path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "status name unavailable")
            })?,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    #[cfg(windows)]
    crate::profile::reject_handle_reparse(&file)?;
    let length = file.metadata()?.len();
    if length == 0 || length > MAX_HARNESS_STATUS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid harness status record size",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_HARNESS_STATUS_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_HARNESS_STATUS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "harness status record too large",
        ));
    }
    let record: HarnessStatusRecord = serde_json::from_slice(&bytes).map_err(invalid_data)?;
    record.validate().map_err(invalid_data)?;
    Ok(Some(record))
}

/// Loads a validated record and classifies only its publication freshness. `recent` does not prove
/// process liveness; profile locking remains the ownership authority.
///
/// # Errors
/// Propagates bounded status-record loading failures.
pub fn load_harness_status_view(path: &Path) -> io::Result<Option<HarnessStatusView>> {
    let Some(record) = load_harness_status(path)? else {
        return Ok(None);
    };
    let now = time::OffsetDateTime::now_utc();
    Ok(Some(status_view_at(record, now)))
}

fn status_view_at(record: HarnessStatusRecord, now: time::OffsetDateTime) -> HarnessStatusView {
    let age = now - record.observed_at.value();
    let freshness = if record.phase == ActivationPhase::Stopped {
        HarnessStatusFreshness::Stopped
    } else if age < -STATUS_FUTURE_SKEW || age > STATUS_STALE_AFTER {
        HarnessStatusFreshness::Stale
    } else {
        HarnessStatusFreshness::Recent
    };
    HarnessStatusView { record, freshness }
}

/// Derives the canonical status path for one profile.
///
/// # Errors
/// Propagates invalid path derivation from the already-validated profile paths.
pub fn harness_status_path(paths: &ProfilePaths) -> Result<PathBuf, ConfigError> {
    paths.harness_status()
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActivationFuture, ActivationHost, ActivationMachine, ActivationPolicy, ActivationSource,
        ActivationTurn, HostFailure, ObservationFailure, WakeMetadata,
    };
    use std::sync::Mutex as StdMutex;
    use tokio::sync::Notify;

    fn test_paths(root: &Path) -> ProfilePaths {
        ProfilePaths {
            metadata: root.join("profiles/alpha.json"),
            credential: root.join("credentials/alpha.json"),
            lock: root.join("locks/alpha.lock"),
        }
    }

    fn timestamp() -> ApiTimestamp {
        ApiTimestamp::new(time::OffsetDateTime::UNIX_EPOCH).unwrap()
    }

    struct OneWakeSource(StdMutex<Option<WakeMetadata>>);

    impl ActivationSource for OneWakeSource {
        fn observe(
            &self,
            _maximum_wait: Duration,
        ) -> ActivationFuture<'_, Result<Option<WakeMetadata>, ObservationFailure>> {
            Box::pin(async move {
                if let Some(wake) = self.0.lock().unwrap().take() {
                    Ok(Some(wake))
                } else {
                    std::future::pending().await
                }
            })
        }
    }

    struct HeldHost {
        started: Arc<Notify>,
        complete: Arc<Notify>,
    }

    impl ActivationHost for HeldHost {
        fn start<'a>(
            &'a self,
            _wake: &'a WakeMetadata,
        ) -> ActivationFuture<'a, Result<Box<dyn ActivationTurn>, HostFailure>> {
            Box::pin(async move {
                self.started.notify_one();
                Ok(Box::new(HeldTurn(Arc::clone(&self.complete))) as Box<dyn ActivationTurn>)
            })
        }
    }

    struct HeldTurn(Arc<Notify>);

    impl ActivationTurn for HeldTurn {
        fn completed(self: Box<Self>) -> ActivationFuture<'static, Result<(), HostFailure>> {
            Box::pin(async move {
                self.0.notified().await;
                Ok(())
            })
        }
    }

    #[test]
    fn status_round_trips_without_message_or_secret_material() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profile.harness-v1.json");
        let mut machine = ActivationMachine::new(ActivationPolicy::default()).unwrap();
        let wake = WakeMetadata::new(
            "alpha".into(),
            "builders".into(),
            3,
            MessagePriorityDto::High,
            "msg_oldest".into(),
        )
        .unwrap();
        machine.observe(Some(wake));
        let record = HarnessStatusRecord::from_snapshot(
            "alpha".into(),
            HarnessAdapterKind::ClaudeChannel,
            &machine.snapshot(),
            timestamp(),
        )
        .unwrap();
        store_harness_status(&path, &record).unwrap();
        assert_eq!(load_harness_status(&path).unwrap(), Some(record));
        let text = fs::read_to_string(path).unwrap().to_ascii_lowercase();
        for forbidden in ["authorization", "bearer", "credential", "token", "body"] {
            assert!(!text.contains(forbidden));
        }
    }

    #[test]
    fn status_rejects_impossible_phase_and_bounded_record_failures() {
        let snapshot = ActivationSnapshot {
            phase: ActivationPhase::Running,
            pending: None,
            reconcile_needed: false,
            retry_attempt: 0,
        };
        assert!(
            HarnessStatusRecord::from_snapshot(
                "alpha".into(),
                HarnessAdapterKind::CodexAppServer,
                &snapshot,
                timestamp(),
            )
            .is_err()
        );

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("status.json");
        fs::write(&path, []).unwrap();
        assert!(load_harness_status(&path).is_err());
        fs::write(
            &path,
            vec![b'x'; usize::try_from(MAX_HARNESS_STATUS_BYTES + 1).unwrap()],
        )
        .unwrap();
        assert!(load_harness_status(&path).is_err());
        fs::write(&path, br#"{"version":2}"#).unwrap();
        assert!(load_harness_status(&path).is_err());
    }

    #[test]
    fn freshness_is_bounded_and_never_claims_liveness() {
        let machine = ActivationMachine::new(ActivationPolicy::default()).unwrap();
        let record = HarnessStatusRecord::from_snapshot(
            "alpha".into(),
            HarnessAdapterKind::CodexAppServer,
            &machine.snapshot(),
            timestamp(),
        )
        .unwrap();
        assert_eq!(
            status_view_at(
                record.clone(),
                time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(30),
            )
            .freshness,
            HarnessStatusFreshness::Recent
        );
        assert_eq!(
            status_view_at(
                record,
                time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(91),
            )
            .freshness,
            HarnessStatusFreshness::Stale
        );

        let stopped = ActivationSnapshot {
            phase: ActivationPhase::Stopped,
            pending: None,
            reconcile_needed: false,
            retry_attempt: 0,
        };
        let record = HarnessStatusRecord::from_snapshot(
            "alpha".into(),
            HarnessAdapterKind::CodexAppServer,
            &stopped,
            timestamp(),
        )
        .unwrap();
        assert_eq!(
            status_view_at(
                record,
                time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(1),
            )
            .freshness,
            HarnessStatusFreshness::Stopped
        );
    }

    #[test]
    fn status_path_is_profile_keyed_and_sibling_to_metadata() {
        let paths = ProfilePaths {
            metadata: PathBuf::from("profiles/alpha-0123456789abcdef.json"),
            credential: PathBuf::from("credentials/alpha-0123456789abcdef.json"),
            lock: PathBuf::from("locks/alpha-0123456789abcdef.lock"),
        };
        assert_eq!(
            harness_status_path(&paths).unwrap(),
            PathBuf::from("profiles/alpha-0123456789abcdef.harness-v1.json")
        );
    }

    #[tokio::test]
    async fn publisher_tracks_running_and_durably_records_clean_stop() {
        let directory = tempfile::tempdir().unwrap();
        let paths = test_paths(directory.path());
        let wake = WakeMetadata::new(
            "alpha".into(),
            "builders".into(),
            1,
            MessagePriorityDto::Normal,
            "msg_one".into(),
        )
        .unwrap();
        let started = Arc::new(Notify::new());
        let complete = Arc::new(Notify::new());
        let activation = Arc::new(
            crate::ActivationRuntime::start(
                Arc::new(OneWakeSource(StdMutex::new(Some(wake)))),
                Arc::new(HeldHost {
                    started: Arc::clone(&started),
                    complete,
                }),
                ActivationPolicy::default(),
            )
            .unwrap(),
        );
        let publisher = HarnessStatusPublisher::start(
            Arc::clone(&activation),
            &paths,
            "alpha".into(),
            HarnessAdapterKind::CodexAppServer,
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .unwrap();
        let status_path = harness_status_path(&paths).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if load_harness_status(&status_path)
                    .unwrap()
                    .is_some_and(|record| record.phase() == ActivationPhase::Running)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();

        activation.shutdown().await;
        publisher.shutdown().await.unwrap();
        let stopped = load_harness_status(&status_path).unwrap().unwrap();
        assert_eq!(stopped.phase(), ActivationPhase::Stopped);
        assert_eq!(stopped.pending_count(), None);
    }

    #[cfg(unix)]
    #[test]
    fn status_target_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.json");
        let link = directory.path().join("status.json");
        fs::write(&target, b"sentinel").unwrap();
        symlink(&target, &link).unwrap();
        assert!(load_harness_status(&link).is_err());
        assert_eq!(fs::read(target).unwrap(), b"sentinel");
    }
}
