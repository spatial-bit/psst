use serde_json::Value;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

#[test]
fn unified_psst_binary_serves_the_protocol_only_mcp_child() {
    let directory = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_psst"));
    command
        .args(["internal", "mcp"])
        .env_remove("PSST_CONFIG")
        .env_remove("PSST_PROFILE")
        .env_remove("PSST_RELAY")
        .env("PSST_PROFILE", "unified-process-test")
        .env("PSST_RELAY", "http://127.0.0.1:9")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_platform_roots(&mut command, directory.path());
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"unified-test","version":"1"}}}
"#,
        )
        .unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output.status);
    assert!(output.stderr.is_empty());
    let line = output.stdout.split(|byte| *byte == b'\n').next().unwrap();
    let response: Value = serde_json::from_slice(line).unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["serverInfo"]["name"], "psst-mcp");
    assert!(response["result"]["capabilities"]["tools"].is_object());
}

fn isolate_platform_roots(command: &mut Command, root: &std::path::Path) {
    std::fs::create_dir_all(root).unwrap();
    #[cfg(windows)]
    {
        command.env("APPDATA", root.join("roaming"));
        command.env("LOCALAPPDATA", root.join("local"));
        std::fs::create_dir_all(root.join("roaming")).unwrap();
        std::fs::create_dir_all(root.join("local")).unwrap();
    }
    #[cfg(target_os = "macos")]
    {
        command.env("HOME", root);
        std::fs::create_dir_all(root.join("Library/Application Support/psst/runtime")).unwrap();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        command.env("HOME", root);
        command.env("XDG_CONFIG_HOME", root.join("config"));
        command.env("XDG_DATA_HOME", root.join("data"));
        command.env("XDG_RUNTIME_DIR", root.join("runtime"));
        std::fs::create_dir_all(root.join("config/psst")).unwrap();
        std::fs::create_dir_all(root.join("data/psst")).unwrap();
        std::fs::create_dir_all(root.join("runtime/psst")).unwrap();
    }
}
