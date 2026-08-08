use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[test]
fn real_children_handle_targeted_platform_interrupt_and_refuse_after_clean_exit() {
    run_child(false, false);
    run_child(true, false);
    run_child(true, true);
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
