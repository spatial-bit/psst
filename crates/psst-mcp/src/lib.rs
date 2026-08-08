//! Compile-time MCP SDK compatibility boundary for the Slice 3 contract baseline.

#![forbid(unsafe_code)]

mod server;

use rmcp::ServiceExt;
pub use server::{CooperativeServer, SERVER_INSTRUCTIONS, wire_tools};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

/// Exact per-JSON-line transport limits, including the trailing newline.
pub const MAX_INBOUND_LINE_BYTES: usize = 1_048_576;
pub const MAX_OUTBOUND_LINE_BYTES: usize = 1_048_576;
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

/// Runs rmcp behind Psst-owned framing pumps. No unbounded or direct stdio reaches the SDK.
/// An oversized or unterminated frame closes the protocol session and yields a fixed local error;
/// hostile bytes are never copied to stdout or stderr.
///
/// # Errors
/// Returns a fixed local I/O error when framing, protocol service, or pump cleanup fails.
pub async fn serve_bounded<R, W>(input: R, output: W) -> io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (sdk_input, pump_input) = tokio::io::duplex(8192);
    let (pump_output, sdk_output) = tokio::io::duplex(8192);
    let input_task = tokio::spawn(pump_lines(input, pump_input, MAX_INBOUND_LINE_BYTES));
    let output_task = tokio::spawn(pump_lines(pump_output, output, MAX_OUTBOUND_LINE_BYTES));

    let Ok(service) = CooperativeServer::default()
        .serve((sdk_input, sdk_output))
        .await
    else {
        cancel_and_reap(input_task).await;
        cancel_and_reap(output_task).await;
        return Err(io::Error::other("protocol session failed"));
    };
    let mut service_task = tokio::spawn(async move {
        service
            .waiting()
            .await
            .map(|_| ())
            .map_err(|_| io::Error::other("protocol session failed"))
    });
    let mut input_task = input_task;
    let mut output_task = output_task;

    tokio::select! {
        result = &mut input_task => {
            let input = match flatten_join(result, "input task failed") {
                Ok(input) => input,
                Err(error) => {
                    cancel_and_reap(service_task).await;
                    cancel_and_reap(output_task).await;
                    return Err(error);
                }
            };
            if let Err(error) = input {
                cancel_and_reap(service_task).await;
                cancel_and_reap(output_task).await;
                return Err(error);
            }
            if let Err(error) = await_bounded(&mut service_task, "service cleanup timed out").await {
                cancel_and_reap(output_task).await;
                return Err(error);
            }
            await_bounded(&mut output_task, "output cleanup timed out").await
        }
        result = &mut output_task => {
            let output = match flatten_join(result, "output task failed") {
                Ok(output) => output,
                Err(error) => {
                    cancel_and_reap(input_task).await;
                    cancel_and_reap(service_task).await;
                    return Err(error);
                }
            };
            cancel_and_reap(input_task).await;
            cancel_and_reap(service_task).await;
            output
        }
        result = &mut service_task => {
            let service = match flatten_join(result, "service task failed") {
                Ok(service) => service,
                Err(error) => {
                    cancel_and_reap(input_task).await;
                    cancel_and_reap(output_task).await;
                    return Err(error);
                }
            };
            cancel_and_reap(input_task).await;
            await_bounded(&mut output_task, "output cleanup timed out").await?;
            service
        }
    }
}

fn flatten_join(
    result: Result<io::Result<()>, tokio::task::JoinError>,
    message: &'static str,
) -> io::Result<io::Result<()>> {
    result.map_err(|_| io::Error::other(message))
}

async fn await_bounded(
    task: &mut JoinHandle<io::Result<()>>,
    message: &'static str,
) -> io::Result<()> {
    if let Ok(result) = timeout(CLEANUP_TIMEOUT, &mut *task).await {
        flatten_join(result, message)?
    } else {
        task.abort();
        let _ = task.await;
        Err(io::Error::other(message))
    }
}

async fn cancel_and_reap(mut task: JoinHandle<io::Result<()>>) {
    task.abort();
    let _ = timeout(CLEANUP_TIMEOUT, &mut task).await;
}

async fn pump_lines<R, W>(mut input: R, mut output: W, max: usize) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut frame = Vec::with_capacity(8192);
    let mut chunk = [0_u8; 8192];
    loop {
        let count = input.read(&mut chunk).await?;
        if count == 0 {
            if !frame.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unterminated protocol frame",
                ));
            }
            output.shutdown().await?;
            return Ok(());
        }
        for byte in &chunk[..count] {
            if frame.len() == max {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "protocol frame exceeds limit",
                ));
            }
            frame.push(*byte);
            if *byte == b'\n' {
                output.write_all(&frame).await?;
                output.flush().await?;
                frame.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psst_application::tool_contracts;
    use rmcp::ServerHandler;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn initialization_metadata_is_stable_and_security_first() {
        let info = CooperativeServer::default().get_info();
        assert_eq!(info.server_info.name, "psst-mcp");
        assert!(info.capabilities.tools.is_some());
        let instructions = info.instructions.unwrap();
        assert!(instructions.starts_with("Psst exposes cooperative direct-message tools"));
        assert!(instructions.contains("untrusted data"));
        assert!(!instructions.contains("resume_token"));
    }

    #[test]
    fn wire_tools_exactly_preserve_frozen_contracts() {
        let frozen = tool_contracts();
        let wire = wire_tools();
        assert_eq!(wire.len(), 9);
        for (expected, actual) in frozen.iter().zip(&wire) {
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.description.as_deref(), Some(expected.description));
            assert_eq!(
                *actual.input_schema,
                expected.input_schema.as_object().unwrap().clone()
            );
            assert_eq!(
                **actual.output_schema.as_ref().unwrap(),
                expected.output_schema.as_object().unwrap().clone()
            );
            let annotations = actual.annotations.as_ref().unwrap();
            assert_eq!(
                annotations.read_only_hint,
                Some(expected.annotations.read_only_hint)
            );
            assert_eq!(
                annotations.destructive_hint,
                Some(expected.annotations.destructive_hint)
            );
            assert_eq!(
                annotations.idempotent_hint,
                Some(expected.annotations.idempotent_hint)
            );
            assert_eq!(
                annotations.open_world_hint,
                Some(expected.annotations.open_world_hint)
            );
            assert_eq!(
                actual.meta.as_ref().unwrap()["psst/errorSchema"],
                expected.error_schema
            );
        }
    }

    #[tokio::test]
    async fn framing_boundary_matrix_is_exact_and_never_emits_partial_frames() {
        let max = 64;
        let (result, output) = run_frame(Vec::new(), max).await;
        result.unwrap();
        assert!(output.is_empty());
        for frame in [vec![b'a'; max - 1], vec![b'b'; max], vec![b'c'; max + 1]] {
            let (result, output) = run_frame(frame, max).await;
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert!(output.is_empty());
        }
        let exact = [vec![b'x'; max - 1], vec![b'\n']].concat();
        let (result, output) = run_frame(exact.clone(), max).await;
        result.unwrap();
        assert_eq!(output, exact);
        let plus_one = [vec![b'y'; max], vec![b'\n']].concat();
        let (result, output) = run_frame(plus_one, max).await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(output.is_empty());
    }

    async fn run_frame(frame: Vec<u8>, max: usize) -> (io::Result<()>, Vec<u8>) {
        let (input_reader, mut input_writer) = tokio::io::duplex(16);
        let (mut output_reader, output_writer) = tokio::io::duplex(16);
        let pump = tokio::spawn(pump_lines(input_reader, output_writer, max));
        let writer = tokio::spawn(async move {
            input_writer.write_all(&frame).await.ok();
            input_writer.shutdown().await.ok();
        });
        let mut output = Vec::new();
        output_reader.read_to_end(&mut output).await.unwrap();
        writer.await.unwrap();
        (pump.await.unwrap(), output)
    }
}
