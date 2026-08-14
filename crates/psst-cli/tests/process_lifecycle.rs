use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write as _},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[test]
fn real_cli_replays_orphaned_confirmed_leave_and_reports_intent_unknown() {
    for phase in ["confirmed", "intent"] {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("client-state");
        let roots = test_platform_paths(&state);
        let origin = "http://127.0.0.1:9";
        let paths = psst_application::ProfilePaths::for_profile(&roots, origin, "default").unwrap();
        let mut journal_name = paths.metadata.file_stem().unwrap().to_os_string();
        journal_name.push(".leave-v1.json");
        let journal = paths.metadata.with_file_name(journal_name);
        let confirmed_at = (phase == "confirmed")
            .then_some(serde_json::json!("2026-08-08T01:02:04.005Z"))
            .unwrap_or(Value::Null);
        let record = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "phase": phase,
            "relay_origin": origin,
            "profile": "default",
            "squad_name": "alpha",
            "squad_id": "sqd_alpha",
            "member_id": "mem_worker",
            "operation_id": "fixed-operation-0",
            "created_at": "2026-08-08T01:02:03.004Z",
            "confirmed_at": confirmed_at,
        }))
        .unwrap();
        write_restricted_fixture(&journal, &record);
        let missing_config = directory.path().join("missing.yaml");
        let mut command = Command::new(env!("CARGO_BIN_EXE_psst"));
        command
            .args([
                "--json",
                "--relay",
                origin,
                "--profile",
                "default",
                "--config",
            ])
            .arg(&missing_config)
            .arg("status")
            .stdin(Stdio::null());
        configure_client_roots(&mut command, &state);
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let failure: Value = serde_json::from_slice(&output.stderr).unwrap();
        if phase == "confirmed" {
            assert_eq!(failure["error"]["code"], "profile_unbound");
            assert!(!journal.exists());
        } else {
            assert_eq!(failure["error"]["code"], "outcome_unknown");
            assert!(journal.exists());
        }
        assert!(!paths.metadata.exists());
    }
}

#[cfg(windows)]
fn test_platform_paths(state: &std::path::Path) -> psst_application::PlatformPaths {
    psst_application::PlatformPaths {
        config_dir: state.join("roaming/psst"),
        data_dir: state.join("local/psst"),
        runtime_dir: state.join("local/psst/runtime"),
    }
}

#[cfg(target_os = "macos")]
fn test_platform_paths(state: &std::path::Path) -> psst_application::PlatformPaths {
    let root = state.join("home/Library/Application Support/psst");
    psst_application::PlatformPaths {
        config_dir: root.clone(),
        data_dir: root.clone(),
        runtime_dir: root.join("runtime"),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn test_platform_paths(state: &std::path::Path) -> psst_application::PlatformPaths {
    psst_application::PlatformPaths {
        config_dir: state.join("config/psst"),
        data_dir: state.join("data/psst"),
        runtime_dir: state.join("runtime/psst"),
    }
}

fn write_restricted_fixture(path: &std::path::Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    #[cfg(windows)]
    let mut file = psst_platform_security::create_restricted_file(
        path,
        &psst_platform_security::current_process_sid().unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap()
    };
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn real_children_handle_targeted_platform_interrupt_and_refuse_after_clean_exit() {
    run_child(false, false);
    run_child(true, false);
    run_child(true, true);
}

#[test]
fn real_cli_profiles_exchange_replay_acknowledge_and_page_messages() {
    let directory = tempfile::tempdir().unwrap();
    let address = unused_loopback();
    let origin = format!("http://{address}");
    let relay_data = directory.path().join("relay");
    let missing_config = directory.path().join("missing.yaml");
    let mut relay = Command::new(env!("CARGO_BIN_EXE_psst"));
    relay
        .args([
            "--json",
            "relay",
            "start",
            "--bind",
            &address.to_string(),
            "--data-dir",
        ])
        .arg(&relay_data)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_signal_group(&mut relay);
    let mut relay = ChildGuard(relay.spawn().unwrap());
    let (ready, stdout_reader) = spawn_json_reader(relay.stdout.take().unwrap());
    let ready: Value =
        serde_json::from_str(&ready.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    assert_eq!(ready["data"]["running"], true);
    let stderr_reader = spawn_reader(relay.stderr.take().unwrap());

    let state = directory.path().join("client-state");
    run_message_journey(&origin, &missing_config, &state);
    wait_for_member_offline(&origin, &missing_config, &state, "bob");
    let offline = run_json_cli(
        &origin,
        "alice",
        &missing_config,
        &state,
        &["message", "send", "--to", "bob", "--body", "offline"],
    );
    let offline_id = offline["data"]["message"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    relay.0.kill().unwrap();
    relay.0.wait().unwrap();
    let _ = stdout_reader.join().unwrap();
    let _ = stderr_reader.join().unwrap();

    let mut restarted = spawn_json_relay(address, &relay_data);
    let (ready, stdout_reader) = spawn_json_reader(restarted.stdout.take().unwrap());
    let ready: Value =
        serde_json::from_str(&ready.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    assert_eq!(ready["data"]["running"], true);
    let stderr_reader = spawn_reader(restarted.stderr.take().unwrap());
    let resumed = run_json_cli(
        &origin,
        "bob",
        &missing_config,
        &state,
        &["inbox", "--limit", "100"],
    );
    assert!(
        resumed["data"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["id"] == offline_id)
    );
    acknowledge_inbox(&origin, &missing_config, &state, "bob", &resumed);
    verify_listen_lifecycle(&origin, &missing_config, &state);
    assert_eq!(
        run_json_cli(&origin, "alice", &missing_config, &state, &["status"])["data"]["health"],
        "ready"
    );
    verify_archive_failure_releases_profile(&origin, &missing_config, &state);
    verify_secret_is_confined(&state, 2);
    assert_eq!(
        run_json_cli(&origin, "bob", &missing_config, &state, &["squad", "leave"])["ok"],
        true
    );
    assert_eq!(
        run_json_cli(
            &origin,
            "alice",
            &missing_config,
            &state,
            &["squad", "archive", "alpha"],
        )["ok"],
        true
    );
    verify_secret_is_confined(&state, 1);
    restarted.0.kill().unwrap();
    restarted.0.wait().unwrap();
    let _ = stdout_reader.join().unwrap();
    let _ = stderr_reader.join().unwrap();
}

fn verify_archive_failure_releases_profile(
    origin: &str,
    config: &std::path::Path,
    state: &std::path::Path,
) {
    let wrong_archive = run_json_cli_failure(
        origin,
        "alice",
        config,
        state,
        &["squad", "archive", "not-alpha"],
        None,
    );
    assert_eq!(wrong_archive["error"]["code"], "authority_denied");
    assert_eq!(
        run_json_cli(origin, "alice", config, state, &["status"])["data"]["health"],
        "ready",
        "authority failure must await shutdown and immediately release the profile lock"
    );
}

fn acknowledge_inbox(
    origin: &str,
    config: &std::path::Path,
    state: &std::path::Path,
    profile: &str,
    inbox: &Value,
) {
    let ids = inbox["data"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return;
    }
    let mut owned = vec!["message".to_owned(), "acknowledge".to_owned()];
    owned.extend(ids);
    let borrowed = owned.iter().map(String::as_str).collect::<Vec<_>>();
    run_json_cli(origin, profile, config, state, &borrowed);
}

fn verify_listen_lifecycle(origin: &str, config: &std::path::Path, state: &std::path::Path) {
    let mut listener = spawn_profile_cli(origin, "bob", config, state, &["listen", "--wait", "1"]);
    wait_for_profile_lock(&mut listener, origin, "bob", config, state);
    let sent = run_json_cli(
        origin,
        "alice",
        config,
        state,
        &["message", "send", "--to", "bob", "--body", "listen-replay"],
    );
    let id = sent["data"]["message"]["id"].as_str().unwrap().to_owned();
    let received = finish_json_child(&mut listener, Duration::from_secs(15), "listen receive");
    assert_cli_envelope(&received, true, "listen");
    assert_eq!(received["data"]["messages"][0]["id"], id);
    let replay = run_json_cli(origin, "bob", config, state, &["inbox", "--limit", "1"]);
    assert_eq!(replay["data"]["messages"][0]["id"], id);
    acknowledge_inbox(origin, config, state, "bob", &replay);

    let mut cancelled = spawn_profile_cli(origin, "bob", config, state, &["listen", "--wait", "1"]);
    wait_for_profile_lock(&mut cancelled, origin, "bob", config, state);
    let first_seen = member_last_seen(origin, config, state, "bob");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && member_last_seen(origin, config, state, "bob") == first_seen
    {
    }
    assert_ne!(
        member_last_seen(origin, config, state, "bob"),
        first_seen,
        "listen heartbeat did not advance last_seen"
    );
    send_interrupt(&cancelled);
    let stopped = finish_json_child(&mut cancelled, Duration::from_secs(10), "listen interrupt");
    assert_cli_envelope(&stopped, true, "listen");
    assert!(stopped["data"]["messages"].as_array().unwrap().is_empty());
}

fn member_last_seen(
    origin: &str,
    config: &std::path::Path,
    state: &std::path::Path,
    name: &str,
) -> String {
    let roster = run_json_cli(origin, "alice", config, state, &["squad", "roster"]);
    roster["data"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["name"] == name)
        .unwrap()["last_seen_at"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn wait_for_profile_lock(
    listener: &mut Child,
    origin: &str,
    profile: &str,
    _config: &std::path::Path,
    state: &std::path::Path,
) {
    let roots = test_platform_paths(state);
    let lock = psst_application::ProfilePaths::for_profile(&roots, origin, profile)
        .unwrap()
        .lock;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        assert!(
            listener.try_wait().unwrap().is_none(),
            "listen exited before acquiring its profile lock"
        );
        // Every lifecycle test gets a fresh platform root, so only this listener can create the
        // profile-specific lock file. Observing it avoids racing the listener for its kernel lock.
        if lock.is_file() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("listen did not acquire its profile lock")
}

fn spawn_json_relay(address: SocketAddr, relay_data: &std::path::Path) -> ChildGuard {
    let mut command = Command::new(env!("CARGO_BIN_EXE_psst"));
    command
        .args([
            "--json",
            "relay",
            "start",
            "--bind",
            &address.to_string(),
            "--data-dir",
        ])
        .arg(relay_data)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_signal_group(&mut command);
    ChildGuard(command.spawn().unwrap())
}

fn wait_for_member_offline(
    origin: &str,
    config: &std::path::Path,
    state: &std::path::Path,
    name: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline {
        let roster = run_json_cli(origin, "alice", config, state, &["squad", "roster"]);
        if roster["data"]["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|member| member["name"] == name && member["presence"] == "offline")
        {
            return;
        }
    }
    panic!("member did not become offline before the advertised lease bound")
}

fn run_message_journey(origin: &str, missing_config: &std::path::Path, state: &std::path::Path) {
    let alice = run_json_cli(
        origin,
        "alice",
        missing_config,
        state,
        &[
            "squad",
            "join",
            "alpha",
            "--name",
            "alice",
            "--role",
            "sender",
            "--mission",
            "test mission",
        ],
    );
    assert_eq!(alice["ok"], true);
    let bob = run_json_cli(
        origin,
        "bob",
        missing_config,
        state,
        &[
            "squad", "join", "alpha", "--name", "bob", "--role", "receiver",
        ],
    );
    assert_eq!(bob["ok"], true);
    let sent = run_json_cli_with_input(
        origin,
        "alice",
        missing_config,
        state,
        &[
            "message",
            "send",
            "--to",
            "bob",
            "--file",
            "-",
            "--correlation-id",
            "journey",
        ],
        Some(b"hello"),
    );
    let message_id = sent["data"]["message"]["id"].as_str().unwrap().to_owned();

    let first = run_json_cli(
        origin,
        "bob",
        missing_config,
        state,
        &["inbox", "--limit", "1"],
    );
    let second = run_json_cli(
        origin,
        "bob",
        missing_config,
        state,
        &["inbox", "--limit", "1"],
    );
    assert_eq!(first["data"]["messages"][0]["id"], message_id);
    assert_eq!(second["data"]["messages"][0]["id"], message_id);
    let acknowledged = run_json_cli(
        origin,
        "bob",
        missing_config,
        state,
        &["message", "acknowledge", &message_id],
    );
    assert_eq!(acknowledged["data"]["acknowledged_ids"][0], message_id);
    let empty = run_json_cli(
        origin,
        "bob",
        missing_config,
        state,
        &["inbox", "--wait", "0"],
    );
    assert!(empty["data"]["messages"].as_array().unwrap().is_empty());
    let transcript = run_json_cli(
        origin,
        "alice",
        missing_config,
        state,
        &["transcript", "--after", "0", "--limit", "1"],
    );
    assert_eq!(transcript["data"]["messages"][0]["id"], message_id);
    verify_process_input_bounds(origin, state);
}

fn verify_process_input_bounds(origin: &str, state: &std::path::Path) {
    std::fs::create_dir_all(state).unwrap();
    let config = state.join("small.yaml");
    let exact = state.join("exact.txt");
    let oversized = state.join("oversized.txt");
    let invalid = state.join("invalid.txt");
    std::fs::write(&config, "max_message_bytes: 5\n").unwrap();
    std::fs::write(&exact, b"12345").unwrap();
    std::fs::write(&oversized, b"123456").unwrap();
    std::fs::write(&invalid, [0xff]).unwrap();
    for arguments in [
        vec!["message", "send", "--to", "bob", "--body", "12345"],
        vec![
            "message",
            "send",
            "--to",
            "bob",
            "--file",
            exact.to_str().unwrap(),
        ],
    ] {
        assert_eq!(
            run_json_cli(origin, "alice", &config, state, &arguments)["ok"],
            true
        );
    }
    for (arguments, input, code) in [
        (
            vec!["message", "send", "--to", "bob", "--body", "123456"],
            None,
            "payload_too_large",
        ),
        (
            vec![
                "message",
                "send",
                "--to",
                "bob",
                "--file",
                oversized.to_str().unwrap(),
            ],
            None,
            "payload_too_large",
        ),
        (
            vec![
                "message",
                "send",
                "--to",
                "bob",
                "--file",
                invalid.to_str().unwrap(),
            ],
            None,
            "invalid_input",
        ),
        (
            vec!["message", "send", "--to", "bob", "--file", "-"],
            Some(&b"123456"[..]),
            "payload_too_large",
        ),
        (
            vec!["message", "send", "--to", "bob", "--file", "-"],
            Some(&[0xff][..]),
            "invalid_input",
        ),
    ] {
        assert_eq!(
            run_json_cli_failure(origin, "alice", &config, state, &arguments, input)["error"]["code"],
            code
        );
    }
}

fn run_json_cli(
    origin: &str,
    profile: &str,
    config: &std::path::Path,
    state: &std::path::Path,
    args: &[&str],
) -> Value {
    run_json_cli_with_input(origin, profile, config, state, args, None)
}

fn spawn_profile_cli(
    origin: &str,
    profile: &str,
    config: &std::path::Path,
    state: &std::path::Path,
    args: &[&str],
) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_psst"));
    command
        .args([
            "--json",
            "--relay",
            origin,
            "--profile",
            profile,
            "--config",
        ])
        .arg(config)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_client_roots(&mut command, state);
    configure_signal_group(&mut command);
    command.spawn().unwrap()
}

fn finish_json_child(child: &mut Child, timeout: Duration, stage: &str) -> Value {
    let stdout = spawn_reader(child.stdout.take().unwrap());
    let stderr = spawn_reader(child.stderr.take().unwrap());
    wait_for_exit(child, timeout);
    let status = child.wait().unwrap();
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(
        status.success(),
        "{stage}: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(stderr.is_empty());
    serde_json::from_slice(&stdout).unwrap()
}

fn run_json_cli_with_input(
    origin: &str,
    profile: &str,
    config: &std::path::Path,
    state: &std::path::Path,
    args: &[&str],
    input: Option<&[u8]>,
) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_psst"));
    command
        .args([
            "--json",
            "--relay",
            origin,
            "--profile",
            profile,
            "--config",
        ])
        .arg(config)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_client_roots(&mut command, state);
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        use std::io::Write as _;
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{} {}: {}",
                args.first().copied().unwrap_or("command"),
                args.get(1).copied().unwrap_or(""),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stderr.is_empty());
            let document: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_cli_envelope(&document, true, expected_command(args));
            return document;
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().ok();
    child.wait().ok();
    panic!(
        "CLI stage timed out: {} {}",
        args.first().copied().unwrap_or("command"),
        args.get(1).copied().unwrap_or("")
    );
}

fn run_json_cli_failure(
    origin: &str,
    profile: &str,
    config: &std::path::Path,
    state: &std::path::Path,
    args: &[&str],
    input: Option<&[u8]>,
) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_psst"));
    command
        .args([
            "--json",
            "--relay",
            origin,
            "--profile",
            profile,
            "--config",
        ])
        .arg(config)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_client_roots(&mut command, state);
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        use std::io::Write as _;
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let document: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_cli_envelope(&document, false, expected_command(args));
            return document;
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().ok();
    child.wait().ok();
    panic!("failure CLI stage timed out")
}

fn assert_cli_envelope(document: &Value, ok: bool, command: &str) {
    let object = document.as_object().unwrap();
    assert_eq!(object.len(), 4);
    assert_eq!(document["version"], "psst.cli.v1");
    assert_eq!(document["ok"], ok);
    assert_eq!(document["command"], command);
    assert!(object.contains_key(if ok { "data" } else { "error" }));
    let rendered = serde_json::to_string(document)
        .unwrap()
        .to_ascii_lowercase();
    assert!(!rendered.contains("bearer "));
    assert!(!rendered.contains("authorization"));
    assert!(!rendered.contains("resume_token"));
}

fn verify_secret_is_confined(root: &std::path::Path, expected_credentials: usize) {
    let mut files = Vec::new();
    collect_files(root, &mut files);
    let credentials = files
        .iter()
        .filter_map(|path| {
            let bytes = std::fs::read(path).ok()?;
            let value: Value = serde_json::from_slice(&bytes).ok()?;
            value
                .get("authorization")?
                .as_str()
                .map(|secret| (path.clone(), secret.as_bytes().to_vec()))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        credentials.len(),
        expected_credentials,
        "one live credential per bound profile"
    );
    for credential in credentials {
        let positives = files
            .iter()
            .filter(|path| {
                std::fs::read(path).is_ok_and(|bytes| {
                    bytes
                        .windows(credential.1.len())
                        .any(|window| window == credential.1)
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(positives.len(), 1);
        assert_eq!(positives[0], &credential.0);
        assert!(credential.0.to_string_lossy().contains("credentials"));
    }
}

fn collect_files(path: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
    if path.is_file() {
        files.push(path.to_owned());
    } else if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            collect_files(&entry.path(), files);
        }
    }
}

fn expected_command(args: &[&str]) -> &'static str {
    match (args.first().copied(), args.get(1).copied()) {
        (Some("squad"), Some("join")) => "squad_join",
        (Some("squad"), Some("roster")) => "squad_roster",
        (Some("squad"), Some("leave")) => "squad_leave",
        (Some("squad"), Some("archive")) => "squad_archive",
        (Some("message"), Some("send")) => "message_send",
        (Some("message"), Some("acknowledge")) => "message_acknowledge",
        (Some("inbox"), _) => "inbox",
        (Some("listen"), _) => "listen",
        (Some("transcript"), _) => "transcript",
        (Some("status"), _) => "status",
        _ => panic!("unmapped CLI evidence command"),
    }
}

struct ChildGuard(Child);
impl std::ops::Deref for ChildGuard {
    type Target = Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            self.0.kill().ok();
            self.0.wait().ok();
        }
    }
}

#[cfg(windows)]
fn configure_client_roots(command: &mut Command, state: &std::path::Path) {
    command
        .env("APPDATA", state.join("roaming"))
        .env("LOCALAPPDATA", state.join("local"));
}

#[cfg(not(windows))]
fn configure_client_roots(command: &mut Command, state: &std::path::Path) {
    command
        .env("HOME", state.join("home"))
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env("XDG_DATA_HOME", state.join("data"))
        .env("XDG_RUNTIME_DIR", state.join("runtime"));
}

fn run_child(json: bool, lan: bool) {
    let directory = tempfile::tempdir().unwrap();
    let address = unused_loopback();
    let missing_config = directory.path().join("missing.yaml");
    let relay_data = directory.path().join("relay");
    let address_text = if lan {
        format!("0.0.0.0:{}", address.port())
    } else {
        address.to_string()
    };
    let mut command = Command::new(env!("CARGO_BIN_EXE_psst"));
    command
        .args(["--config", missing_config.to_str().unwrap()])
        .args(json.then_some("--json"))
        .args([
            "relay",
            "start",
            "--bind",
            &address_text,
            "--data-dir",
            relay_data.to_str().unwrap(),
        ])
        .args(lan.then_some("--allow-lan"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_signal_group(&mut command);
    let mut child = command.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let stdout_reader = if json {
        let (line_rx, reader) = spawn_json_reader(stdout);
        let line = line_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let envelope: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(envelope["version"], "psst.cli.v1");
        assert_eq!(envelope["command"], "relay_start");
        assert_eq!(envelope["data"]["running"], true);
        assert_eq!(envelope["data"]["bind"], address_text);
        assert_eq!(envelope["data"]["trusted_lan"], lan);
        if lan {
            assert!(
                envelope["data"]["security_warning"]
                    .as_str()
                    .unwrap()
                    .contains("no TLS")
            );
        } else {
            assert!(envelope["data"]["security_warning"].is_null());
        }
        reader
    } else {
        spawn_reader(stdout)
    };
    let stderr_reader = spawn_reader(stderr);
    wait_until(Duration::from_secs(5), || {
        TcpStream::connect(address).is_ok()
    });
    assert_delayed_http_health(address);
    send_interrupt(&child);
    wait_for_exit(&mut child, Duration::from_secs(5));
    let status = child.wait().unwrap();
    let trailing_stdout = stdout_reader.join().unwrap();
    let diagnostics = stderr_reader.join().unwrap();
    assert!(
        status.success(),
        "{status:?}: {}",
        String::from_utf8_lossy(&diagnostics)
    );
    if json {
        assert!(trailing_stdout.is_empty());
        assert!(diagnostics.is_empty());
    } else {
        assert!(String::from_utf8_lossy(&trailing_stdout).contains("clean"));
        let diagnostics = String::from_utf8_lossy(&diagnostics);
        assert!(diagnostics.contains("relay started"), "{diagnostics}");
        assert!(diagnostics.contains("database"), "{diagnostics}");
        assert!(diagnostics.contains("schema_version"), "{diagnostics}");
    }
    wait_until(Duration::from_secs(5), || {
        TcpStream::connect(address).is_err()
    });
    assert!(relay_data.join("psst.db").is_file());
    assert!(!relay_data.join("psst.db-wal").exists());
}

fn spawn_json_reader(
    reader: impl Read + Send + 'static,
) -> (mpsc::Receiver<String>, thread::JoinHandle<Vec<u8>>) {
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        sender.send(line).ok();
        let mut trailing = Vec::new();
        reader.read_to_end(&mut trailing).unwrap();
        trailing
    });
    (receiver, handle)
}

fn spawn_reader(mut reader: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        bytes
    })
}

#[cfg(windows)]
fn configure_signal_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0000_0200);
}

#[cfg(not(windows))]
fn configure_signal_group(_command: &mut Command) {}

#[cfg(windows)]
fn send_interrupt(child: &Child) {
    // CTRL_BREAK_EVENT can target the child's new process group. CTRL_C_EVENT cannot be safely
    // targeted this way, so broadcasting it would also endanger the Cargo test runner.
    let script = format!(
        "Add-Type -TypeDefinition 'using System.Runtime.InteropServices; public static class C {{ [DllImport(\"kernel32.dll\")] public static extern bool GenerateConsoleCtrlEvent(uint e, uint g); }}'; if (-not [C]::GenerateConsoleCtrlEvent(1, {})) {{ exit 1 }}",
        child.id()
    );
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(not(windows))]
fn send_interrupt(child: &Child) {
    // A targeted SIGINT exercises the Unix Ctrl-C shutdown path.
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

fn wait_for_exit(child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().ok();
    child.wait().ok();
    panic!("child did not exit within {timeout:?}");
}

fn unused_loopback() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    address
}

fn assert_delayed_http_health(address: SocketAddr) {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    thread::sleep(Duration::from_millis(100));
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = [0_u8; 256];
    let count = stream.read(&mut response).unwrap();
    assert!(
        response[..count].starts_with(b"HTTP/1.1 200"),
        "relay accepted TCP but did not service delayed HTTP: {}",
        String::from_utf8_lossy(&response[..count])
    );
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("condition was not met within {timeout:?}");
}
