use psst_client::{Client, ClientConfig, Error, RetryPolicy, Session};
use psst_protocol::{
    AgentModeDto, AvailabilityDto, AvailabilitySourceDto, ClientMetadata, CreateSquadRequest,
    HeartbeatRequest, JoinSquadRequest, MessagePriorityDto, MessageSequence, SendMessageResponse,
};
use psst_relay::{RelayConfig, StoreWorker};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

struct RelayFixture {
    directory: TempDir,
    database: PathBuf,
    probe: StoreWorker,
    shutdown: watch::Sender<bool>,
    server: JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>,
    base: String,
}

impl RelayFixture {
    async fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("reliability.db");
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        Self::launch(directory, database, address).await
    }

    async fn launch(directory: TempDir, database: PathBuf, address: std::net::SocketAddr) -> Self {
        let mut config = RelayConfig::local(&database);
        config.bind = address;
        config.queue_capacity = 512;
        config.max_connections = 128;
        config.max_in_flight_requests = 128;
        config.request_timeout = Duration::from_secs(8);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (probe_tx, probe_rx) = oneshot::channel();
        let server = tokio::spawn(psst_relay::serve_with_reliability_probe(
            config,
            shutdown_rx,
            probe_tx,
        ));
        let probe = probe_rx.await.unwrap();
        let base = format!("http://{address}");
        let health = Client::new(&base, ClientConfig::default()).unwrap();
        for _ in 0..100 {
            if health.health().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        health.health().await.unwrap();
        Self {
            directory,
            database,
            probe,
            shutdown,
            server,
            base,
        }
    }

    async fn stop(self) {
        let _ = self.shutdown.send(true);
        tokio::time::timeout(Duration::from_secs(10), self.server)
            .await
            .expect("production shutdown exceeded ten seconds")
            .unwrap()
            .unwrap();
        assert_listener_refused(&self.base).await;
        psst_store::Store::open(&self.database)
            .unwrap()
            .readiness()
            .unwrap();
    }

    async fn restart(self) -> Self {
        let address = self.base.strip_prefix("http://").unwrap().parse().unwrap();
        let _ = self.shutdown.send(true);
        tokio::time::timeout(Duration::from_secs(10), self.server)
            .await
            .expect("production restart shutdown exceeded ten seconds")
            .unwrap()
            .unwrap();
        assert_listener_refused(&self.base).await;
        psst_store::Store::open(&self.database)
            .unwrap()
            .readiness()
            .unwrap();
        Self::launch(self.directory, self.database, address).await
    }
}

async fn assert_listener_refused(base: &str) {
    let config = ClientConfig {
        connect_timeout: Duration::from_millis(250),
        request_timeout: Duration::from_millis(500),
        retry: RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        },
        ..ClientConfig::default()
    };
    assert!(matches!(
        Client::new(base, config).unwrap().health().await,
        Err(Error::Transport(_))
    ));
}

fn join_request(name: &str) -> JoinSquadRequest {
    JoinSquadRequest {
        name: name.into(),
        role: "reliability".into(),
        mode: AgentModeDto::Cooperative,
        client: ClientMetadata {
            kind: "w207".into(),
            hostname: None,
            version: Some(env!("CARGO_PKG_VERSION").into()),
        },
        mission: None,
    }
}

fn percentile(samples: &mut [Duration], percentile: usize) -> Duration {
    samples.sort_unstable();
    samples[(samples.len() * percentile).div_ceil(100).saturating_sub(1)]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[allow(clippy::too_many_lines)]
async fn one_hundred_watchers_wake_without_loss() {
    const WATCHERS: usize = 100;
    // All waits must remain registered while a slower native runner commits
    // the full bounded delivery burst. Ten seconds was too close to the
    // observed Windows scheduling/SQLite envelope and could let the earliest
    // waiter expire just before its message committed.
    const LONG_POLL_SECONDS: u8 = 30;
    let fixture = RelayFixture::start().await;
    let config = ClientConfig {
        max_in_flight: 128,
        retry: RetryPolicy {
            max_attempts: 2,
            ..RetryPolicy::default()
        },
        ..ClientConfig::default()
    };
    let client = Arc::new(Client::new(&fixture.base, config).unwrap());
    client
        .create_squad(&CreateSquadRequest {
            name: "watchers".into(),
            mission: "one hundred concurrent typed clients".into(),
        })
        .await
        .unwrap();
    let sender = Arc::new(
        client
            .join("watchers", &join_request("sender"))
            .await
            .unwrap(),
    );
    let mut sessions = Vec::with_capacity(WATCHERS);
    for index in 0..WATCHERS {
        sessions.push(
            client
                .join("watchers", &join_request(&format!("watcher-{index:03}")))
                .await
                .unwrap(),
        );
    }

    let (started_tx, mut started_rx) = mpsc::channel(WATCHERS);
    let mut waits = Vec::with_capacity(WATCHERS);
    for (index, session) in sessions.into_iter().enumerate() {
        let client = Arc::clone(&client);
        let started_tx = started_tx.clone();
        waits.push(tokio::spawn(async move {
            started_tx.send(()).await.unwrap();
            let inbox = client
                .inbox(1, LONG_POLL_SECONDS, &session.credential)
                .await
                .unwrap();
            (index, inbox.messages)
        }));
    }
    drop(started_tx);
    for _ in 0..WATCHERS {
        started_rx.recv().await.unwrap();
    }
    let registration_deadline = Instant::now() + Duration::from_secs(5);
    while fixture.probe.reliability_active_inbox_waiters() != WATCHERS {
        assert!(
            Instant::now() < registration_deadline,
            "only {} of {WATCHERS} TCP long polls registered",
            fixture.probe.reliability_active_inbox_waiters()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let mut deliveries = tokio::task::JoinSet::new();
    for index in 0..WATCHERS {
        if deliveries.len() >= 16 {
            deliveries.join_next().await.unwrap().unwrap();
        }
        let client = Arc::clone(&client);
        let sender = Arc::clone(&sender);
        deliveries.spawn(async move {
            client
                .send(
                    format!("watcher-{index:03}"),
                    format!("wake-{index}"),
                    MessagePriorityDto::Normal,
                    None,
                    Some("w207-watchers".into()),
                    &sender.credential,
                )
                .await
                .unwrap();
        });
    }
    while let Some(delivery) = deliveries.join_next().await {
        delivery.unwrap();
    }
    let mut received = vec![false; WATCHERS];
    for wait in waits {
        let (index, messages) = wait.await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].body, format!("wake-{index}"));
        received[index] = true;
    }
    assert!(received.into_iter().all(|value| value));
    assert_eq!(fixture.probe.reliability_active_inbox_waiters(), 0);
    assert_eq!(
        client
            .roster("watchers", &sender.credential)
            .await
            .unwrap()
            .members
            .len(),
        WATCHERS + 1
    );
    for _ in 0..20 {
        client.health().await.unwrap();
    }
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustained_throughput_and_non_waiting_p95_are_measured_without_loss() {
    const OFFERED_RATE: u64 = 105;
    const MAX_CONCURRENCY: usize = 16;
    const WARMUP: usize = 20;
    const LATENCY_SAMPLES: usize = 200;
    let seconds = std::env::var("PSST_W207_PERF_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_u64);
    let repetitions = std::env::var("PSST_W207_PERF_REPETITIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_usize);
    let mut fixture = RelayFixture::start().await;
    for repetition in 0..repetitions {
        let client = Arc::new(Client::new(&fixture.base, ClientConfig::default()).unwrap());
        let evidence = run_measurement(
            &client,
            repetition,
            seconds,
            OFFERED_RATE,
            MAX_CONCURRENCY,
            WARMUP,
            LATENCY_SAMPLES,
        )
        .await;
        drop(client);
        fixture = fixture.restart().await;
        let restarted = Client::new(&fixture.base, ClientConfig::default()).unwrap();
        reconcile_measurement(&restarted, &evidence).await;
    }
    fixture.stop().await;
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_measurement(
    client: &Arc<Client>,
    repetition: usize,
    seconds: u64,
    offered_rate: u64,
    max_concurrency: usize,
    warmup: usize,
    latency_samples: usize,
) -> MeasurementEvidence {
    let squad = format!("measure-{repetition}");
    client
        .create_squad(&CreateSquadRequest {
            name: squad.clone(),
            mission: "open-loop local reliability measurement".into(),
        })
        .await
        .unwrap();
    let sender = Arc::new(client.join(&squad, &join_request("sender")).await.unwrap());
    let recipient = client
        .join(&squad, &join_request("recipient"))
        .await
        .unwrap();
    let mut expected = Vec::new();
    for index in 0..warmup {
        let response = client
            .send(
                "recipient".into(),
                format!("warmup-{index}"),
                MessagePriorityDto::Normal,
                None,
                Some("w207-warmup".into()),
                &sender.credential,
            )
            .await
            .unwrap();
        expected.push((
            response.message.sequence.value(),
            response.message.id,
            format!("warmup-{index}"),
        ));
    }

    let period = Duration::from_nanos(1_000_000_000 / offered_rate);
    let started = Instant::now();
    let mut next_release = tokio::time::Instant::now();
    let mut offered = 0_usize;
    let mut next_heartbeat = Duration::from_secs(5);
    let mut sends: tokio::task::JoinSet<(Duration, usize, SendMessageResponse)> =
        tokio::task::JoinSet::new();
    let mut send_latencies = Vec::new();
    while started.elapsed() < Duration::from_secs(seconds) {
        if started.elapsed() >= next_heartbeat {
            let heartbeat = HeartbeatRequest {
                availability: AvailabilityDto::Busy,
                availability_source: AvailabilitySourceDto::AgentReported,
            };
            client
                .heartbeat(&heartbeat, &sender.credential)
                .await
                .unwrap();
            client
                .heartbeat(&heartbeat, &recipient.credential)
                .await
                .unwrap();
            next_heartbeat += Duration::from_secs(5);
        }
        tokio::time::sleep_until(next_release).await;
        next_release += period;
        while sends.len() >= max_concurrency {
            let (latency, index, response) = sends.join_next().await.unwrap().unwrap();
            send_latencies.push(latency);
            expected.push((
                response.message.sequence.value(),
                response.message.id,
                format!("message-{index}"),
            ));
        }
        let client = Arc::clone(client);
        let sender = Arc::clone(&sender);
        let index = offered;
        sends.spawn(async move {
            let request_started = Instant::now();
            let response = client
                .send(
                    "recipient".into(),
                    format!("message-{index}"),
                    MessagePriorityDto::Normal,
                    None,
                    Some("w207-offered-rate".into()),
                    &sender.credential,
                )
                .await
                .unwrap();
            (request_started.elapsed(), index, response)
        });
        offered += 1;
    }
    while let Some(result) = sends.join_next().await {
        let (latency, index, response) = result.unwrap();
        send_latencies.push(latency);
        expected.push((
            response.message.sequence.value(),
            response.message.id,
            format!("message-{index}"),
        ));
    }
    let elapsed = started.elapsed();

    let mut transcript_messages = Vec::new();
    let mut after = MessageSequence::default();
    loop {
        let page = client
            .transcript(&squad, after, 100, &sender.credential)
            .await
            .unwrap();
        transcript_messages.extend(page.messages.iter().cloned());
        if page.messages.len() < 100 {
            break;
        }
        after = page.next_after.expect("a full page has a continuation");
    }
    expected.sort_unstable_by_key(|item| item.0);
    assert_eq!(transcript_messages.len(), warmup + offered);
    assert_eq!(expected.len(), transcript_messages.len());
    let unique_ids: std::collections::HashSet<_> =
        expected.iter().map(|item| item.1.as_str()).collect();
    assert_eq!(unique_ids.len(), expected.len());
    for (offset, (expected, actual)) in expected.iter().zip(&transcript_messages).enumerate() {
        assert_eq!(expected.0, actual.sequence.value());
        if offset > 0 {
            assert_eq!(
                actual.sequence.value(),
                transcript_messages[offset - 1].sequence.value() + 1
            );
        }
        assert_eq!(expected.1, actual.id);
        assert_eq!(expected.2, actual.body);
    }
    assert_eq!(
        client
            .inbox(100, 0, &recipient.credential)
            .await
            .unwrap()
            .pending_count,
        (warmup + offered) as u64
    );

    let mut inbox_latencies = Vec::with_capacity(latency_samples);
    for _ in 0..latency_samples {
        let request_started = Instant::now();
        client.inbox(1, 0, &recipient.credential).await.unwrap();
        inbox_latencies.push(request_started.elapsed());
    }
    let send_p50 = percentile(&mut send_latencies, 50);
    let send_p95 = percentile(&mut send_latencies, 95);
    let send_p99 = percentile(&mut send_latencies, 99);
    let send_max = *send_latencies.last().unwrap();
    let inbox_p95 = percentile(&mut inbox_latencies, 95);
    let completed_rate = f64::from(u32::try_from(offered).expect("bounded measurement count"))
        / elapsed.as_secs_f64();
    let rate_target_met =
        offered as u64 >= seconds * offered_rate && send_latencies.len() == offered;
    eprintln!(
        "W-207 measurement: repetition={}, profile={}, os={}, arch={}, logical_cpus={}, duration_s={}, offered_rate={}, offered={}, completed_rate={:.1}, max_concurrency={}, sqlite_synchronous=FULL, wal_autocheckpoint_pages=256, send_p50_us={}, send_p95_us={}, send_p99_us={}, send_max_us={}, inbox_wait0_p95_us={}, target_rate_met={}, target_p95_met={}",
        repetition + 1,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
        seconds,
        offered_rate,
        offered,
        completed_rate,
        max_concurrency,
        send_p50.as_micros(),
        send_p95.as_micros(),
        send_p99.as_micros(),
        send_max.as_micros(),
        inbox_p95.as_micros(),
        rate_target_met,
        send_p95 < Duration::from_millis(100) && inbox_p95 < Duration::from_millis(100)
    );
    assert_eq!(send_latencies.len(), offered);
    if !cfg!(debug_assertions) {
        assert!(
            completed_rate >= 100.0,
            "release completed rate {completed_rate:.3}/s"
        );
        assert!(
            send_p95 < Duration::from_millis(100),
            "release send p95 {send_p95:?}"
        );
        assert!(
            inbox_p95 < Duration::from_millis(100),
            "release inbox(wait=0) p95 {inbox_p95:?}"
        );
    }
    MeasurementEvidence {
        squad,
        sender: Arc::into_inner(sender).expect("measurement tasks released sender"),
        recipient,
        expected,
    }
}

struct MeasurementEvidence {
    squad: String,
    sender: Session,
    recipient: Session,
    expected: Vec<(i64, String, String)>,
}

async fn reconcile_measurement(client: &Client, evidence: &MeasurementEvidence) {
    let mut after = MessageSequence::default();
    let mut messages = Vec::new();
    loop {
        let page = client
            .transcript(&evidence.squad, after, 100, &evidence.sender.credential)
            .await
            .unwrap();
        messages.extend(page.messages.iter().cloned());
        if page.messages.len() < 100 {
            break;
        }
        after = page.next_after.unwrap();
    }
    assert_eq!(messages.len(), evidence.expected.len());
    for (index, (expected, actual)) in evidence.expected.iter().zip(messages.iter()).enumerate() {
        assert_eq!(expected.0, actual.sequence.value());
        if index > 0 {
            assert_eq!(
                actual.sequence.value(),
                messages[index - 1].sequence.value() + 1
            );
        }
        assert_eq!(expected.1, actual.id);
        assert_eq!(expected.2, actual.body);
    }
    assert_eq!(
        client
            .inbox(100, 0, &evidence.recipient.credential)
            .await
            .unwrap()
            .pending_count,
        evidence.expected.len() as u64
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_disconnect_and_production_shutdown_release_live_polls() {
    let fixture = RelayFixture::start().await;
    let config = ClientConfig {
        retry: RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        },
        ..ClientConfig::default()
    };
    let client = Arc::new(Client::new(&fixture.base, config.clone()).unwrap());
    client
        .create_squad(&CreateSquadRequest {
            name: "shutdown".into(),
            mission: "disconnect cleanup".into(),
        })
        .await
        .unwrap();
    let first = client
        .join("shutdown", &join_request("first"))
        .await
        .unwrap();
    let disconnected = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.inbox(1, 30, &first.credential).await }
    });
    wait_for_waiters(&fixture, 1).await;
    disconnected.abort();
    assert!(matches!(disconnected.await, Err(error) if error.is_cancelled()));
    wait_for_waiters(&fixture, 0).await;

    let second = client
        .join("shutdown", &join_request("second"))
        .await
        .unwrap();
    let live = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.inbox(1, 30, &second.credential).await }
    });
    wait_for_waiters(&fixture, 1).await;
    let shutdown_started = Instant::now();
    fixture.shutdown.send(true).unwrap();
    let released = live.await.unwrap();
    assert!(matches!(
        released,
        Err(Error::Api {
            status: 503,
            code: psst_protocol::ApiErrorCode::DatabaseBusy,
            retryable: true
        })
    ));
    assert!(shutdown_started.elapsed() < Duration::from_secs(2));
    wait_for_waiters(&fixture, 0).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let refused = Client::new(&fixture.base, config).unwrap().health().await;
    assert!(matches!(refused, Err(Error::Transport(_))));
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)]
async fn held_external_writer_lock_is_bounded_stable_and_recovers() {
    let fixture = RelayFixture::start().await;
    let config = ClientConfig {
        // SQLite's busy handler is bounded at two seconds. Leave ample CI scheduling headroom
        // while keeping the client deadline inside the relay's eight-second worker deadline.
        request_timeout: Duration::from_secs(7),
        retry: RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        },
        ..ClientConfig::default()
    };
    let client = Client::new(&fixture.base, config).unwrap();
    client
        .create_squad(&CreateSquadRequest {
            name: "locked".into(),
            mission: "writer contention".into(),
        })
        .await
        .unwrap();
    let sender = client
        .join("locked", &join_request("sender"))
        .await
        .unwrap();
    let recipient = client
        .join("locked", &join_request("recipient"))
        .await
        .unwrap();
    let prepared = client
        .prepare_send(
            "recipient".into(),
            "recovered".into(),
            MessagePriorityDto::Normal,
            None,
            Some("w207-lock".into()),
        )
        .unwrap();
    assert!(
        client
            .transcript(
                "locked",
                MessageSequence::default(),
                100,
                &sender.credential
            )
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(
        client
            .inbox(100, 0, &recipient.credential)
            .await
            .unwrap()
            .pending_count,
        0
    );

    let database = fixture.database.clone();
    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let lock = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(database).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        acquired_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        connection.execute_batch("ROLLBACK").unwrap();
    });
    acquired_rx.recv().unwrap();
    let started = Instant::now();
    let result = client.send_prepared(&prepared, &sender.credential).await;
    let elapsed = started.elapsed();
    assert!(
        matches!(
            result,
            Err(Error::Api {
                status: 503,
                code: psst_protocol::ApiErrorCode::DatabaseBusy,
                retryable: true
            })
        ),
        "unexpected lock result after {elapsed:?}: {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(7),
        "writer lock exceeded client bound: {elapsed:?}"
    );
    assert!(
        client
            .transcript(
                "locked",
                MessageSequence::default(),
                100,
                &sender.credential
            )
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(
        client
            .inbox(100, 0, &recipient.credential)
            .await
            .unwrap()
            .pending_count,
        0
    );
    release_tx.send(()).unwrap();
    lock.join().unwrap();
    let committed = client
        .send_prepared(&prepared, &sender.credential)
        .await
        .unwrap();
    assert!(!committed.idempotent_replay);
    let replay = client
        .send_prepared(&prepared, &sender.credential)
        .await
        .unwrap();
    assert!(replay.idempotent_replay);
    assert_eq!(replay.message.id, committed.message.id);
    let transcript = client
        .transcript(
            "locked",
            MessageSequence::default(),
            100,
            &sender.credential,
        )
        .await
        .unwrap();
    assert_eq!(transcript.messages.len(), 1);
    assert_eq!(transcript.messages[0].id, committed.message.id);
    assert_eq!(
        client
            .inbox(100, 0, &recipient.credential)
            .await
            .unwrap()
            .pending_count,
        1
    );
    fixture.stop().await;
}

async fn wait_for_waiters(fixture: &RelayFixture, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.probe.reliability_active_inbox_waiters() != expected {
        assert!(
            Instant::now() < deadline,
            "waiter count did not reach {expected}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
