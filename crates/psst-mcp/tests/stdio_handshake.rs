use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Command, Stdio},
    sync::{Mutex, MutexGuard, mpsc},
    thread,
    time::{Duration, Instant},
};

static CHILD_SERIAL: Mutex<()> = Mutex::new(());

#[test]
#[allow(clippy::too_many_lines)] // One ordered transcript proves negotiation through shutdown.
fn pinned_sdk_completes_initialize_ping_and_clean_stdio_shutdown() {
    let (_serial, _root, mut child) = isolated_child("w306-handshake");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = ProtocolReader::new(child.stdout.take().unwrap());

    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "psst-w301-contract-test", "version": "0"}
            }
        })
    )
    .unwrap();
    stdin.flush().unwrap();
    let initialized = stdout.read(&mut child);
    assert_eq!(initialized["jsonrpc"], "2.0");
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["serverInfo"]["name"], "psst-mcp");
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    assert!(
        initialized["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("untrusted data")
    );

    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"ping"})
    )
    .unwrap();
    stdin.flush().unwrap();
    let pong = stdout.read(&mut child);
    assert_eq!(
        pong,
        serde_json::json!({"jsonrpc":"2.0","id":2,"result":{}})
    );

    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}})
    )
    .unwrap();
    stdin.flush().unwrap();
    let listed = stdout.read(&mut child);
    assert_eq!(listed["id"], 3);
    let expected = serde_json::to_value(psst_mcp::wire_tools()).unwrap();
    assert_eq!(listed["result"]["tools"], expected);

    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"squad_list","arguments":{}}})
    )
    .unwrap();
    stdin.flush().unwrap();
    let unavailable = stdout.read(&mut child);
    assert_eq!(unavailable["id"], 4);
    assert_eq!(unavailable["result"]["isError"], true);
    assert_eq!(
        unavailable["result"]["structuredContent"]["error"]["code"],
        "relay_unavailable"
    );
    let tool_text = unavailable["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(tool_text).unwrap(),
        unavailable["result"]["structuredContent"]
    );
    assert!(!tool_text.contains("exit_class"));

    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"not_a_psst_tool","arguments":{}}})
    )
    .unwrap();
    stdin.flush().unwrap();
    let unknown = stdout.read(&mut child);
    assert_eq!(unknown["id"], 5);
    assert_eq!(unknown["error"]["code"], -32602);

    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"squad_join","arguments":{}}})
    )
    .unwrap();
    stdin.flush().unwrap();
    let invalid = stdout.read(&mut child);
    assert_eq!(invalid["id"], 6);
    assert_eq!(invalid["error"]["code"], -32602);

    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":999,"reason":"test cancellation"}})
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":7,"method":"ping"})
    )
    .unwrap();
    stdin.flush().unwrap();
    let after_cancel = stdout.read(&mut child);
    assert_eq!(
        after_cancel,
        serde_json::json!({"jsonrpc":"2.0","id":7,"result":{}})
    );

    drop(stdin);
    wait_for_exit(&mut child, Duration::from_secs(5), Some(&mut stdout));
    let output = child.wait_with_output().unwrap();
    assert!(stdout.finish().is_empty());
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout contained non-protocol bytes after EOF"
    );
    assert!(
        output.stderr.is_empty(),
        "successful handshake emitted diagnostics"
    );
}

#[test]
fn oversized_input_fails_closed_without_reflecting_hostile_content() {
    let (_serial, _root, mut child) = isolated_child("w306-oversized");
    let mut stdin = child.stdin.take().unwrap();
    let hostile = "HOSTILE-CANARY-DO-NOT-REFLECT";
    stdin.write_all(hostile.as_bytes()).unwrap();
    stdin
        .write_all(&vec![b'x'; psst_mcp::MAX_INBOUND_LINE_BYTES])
        .unwrap();
    stdin.write_all(b"\n").ok();
    wait_for_exit(&mut child, Duration::from_secs(5), None);
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(70));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains(hostile));
    assert!(!stderr.contains(hostile));
    assert!(stdout.is_empty());
    assert_eq!(stderr, "psst-mcp: protocol session failed\n");
}

#[test]
fn malformed_json_fails_closed_with_protocol_pure_stdout() {
    let (_serial, _root, mut child) = isolated_child("w306-malformed");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"{not-json}\n").unwrap();
    drop(stdin);
    wait_for_exit(&mut child, Duration::from_secs(5), None);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(70));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"psst-mcp: protocol session failed\n");
}

#[test]
fn unknown_method_is_a_json_rpc_protocol_failure() {
    let (_serial, _root, mut child) = isolated_child("w306-unknown-method");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = ProtocolReader::new(child.stdout.take().unwrap());
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-11-25","capabilities":{},
                "clientInfo":{"name":"failure-test","version":"0"}
            }
        })
    )
    .unwrap();
    stdin.flush().unwrap();
    let initialized = stdout.read(&mut child);
    assert_eq!(initialized["id"], 1);
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"not/a/method"})
    )
    .unwrap();
    stdin.flush().unwrap();
    let failure = stdout.read(&mut child);
    assert_eq!(failure["jsonrpc"], "2.0");
    assert_eq!(failure["id"], 4);
    assert!(failure.get("error").is_some());
    drop(stdin);
    wait_for_exit(&mut child, Duration::from_secs(5), Some(&mut stdout));
    let output = child.wait_with_output().unwrap();
    assert!(stdout.finish().is_empty());
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

fn isolated_child(
    profile: &str,
) -> (
    MutexGuard<'static, ()>,
    tempfile::TempDir,
    std::process::Child,
) {
    // Cooperative startup acquires an OS-backed profile lock whose bounded endpoint namespace can
    // conservatively collide across unrelated profiles. Serialize these child-process protocol
    // tests so startup is deterministic and each assertion reaches the intended stdio boundary.
    let serial = CHILD_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let platform_roots = [root.path().to_path_buf()];
    #[cfg(target_os = "macos")]
    let platform_roots = [root.path().join("Library/Application Support/psst/runtime")];
    #[cfg(all(unix, not(target_os = "macos")))]
    let platform_roots = [
        root.path().join("config/psst"),
        root.path().join("data/psst"),
        root.path().join("runtime/psst"),
    ];
    for path in platform_roots {
        std::fs::create_dir_all(path).unwrap();
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_psst-mcp"));
    for key in [
        "PSST_RELAY",
        "PSST_PROFILE",
        "PSST_RELAY_BIND",
        "PSST_DATA_DIR",
        "PSST_ALLOW_LAN",
        "PSST_LOG",
        "PSST_LOG_FORMAT",
        "PSST_MAX_MESSAGE_BYTES",
        "PSST_MAX_LONG_POLL_SECONDS",
        "PSST_HEARTBEAT_SECONDS",
        "PSST_LEASE_SECONDS",
    ] {
        command.env_remove(key);
    }
    command
        .env("PSST_RELAY", "http://127.0.0.1:9")
        .env("PSST_PROFILE", profile)
        .env("APPDATA", root.path())
        .env("LOCALAPPDATA", root.path())
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    (serial, root, child)
}

fn wait_for_exit(
    child: &mut std::process::Child,
    timeout: Duration,
    mut reader: Option<&mut ProtocolReader>,
) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().ok();
    child.wait().ok();
    if let Some(reader) = &mut reader {
        reader.join_after_termination();
    }
    panic!("MCP child did not exit within {timeout:?}");
}

struct ProtocolReader {
    receiver: mpsc::Receiver<String>,
    thread: Option<thread::JoinHandle<Vec<String>>>,
}

impl ProtocolReader {
    fn new(stdout: impl Read + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut trailing = Vec::new();
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                if sender.send(line.clone()).is_err() {
                    trailing.push(line);
                }
            }
            trailing
        });
        Self {
            receiver,
            thread: Some(thread),
        }
    }

    fn read(&mut self, child: &mut std::process::Child) -> Value {
        let line = match self.receiver.recv_timeout(Duration::from_secs(15)) {
            Ok(line) => line,
            Err(error) => {
                child.kill().ok();
                child.wait().ok();
                if let Some(reader) = self.thread.take() {
                    let _ = reader.join();
                }
                panic!("timed protocol response: {error}");
            }
        };
        serde_json::from_str(&line).unwrap()
    }

    fn finish(mut self) -> Vec<String> {
        let mut remaining = self.thread.take().unwrap().join().unwrap();
        remaining.extend(self.receiver.try_iter());
        remaining
    }

    fn join_after_termination(&mut self) {
        if let Some(reader) = self.thread.take() {
            let _ = reader.join();
        }
    }
}
