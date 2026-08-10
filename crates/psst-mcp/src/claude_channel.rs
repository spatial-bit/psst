use psst_application::{
    ActivationFuture, ActivationHost, ActivationPhase, ActivationPolicy, ActivationRuntime,
    ActivationSource, ActivationTurn, HostFailure, SessionActivationSource, SessionRuntime,
    WakeMetadata,
};
use rmcp::{
    Peer, RoleServer,
    model::{CustomNotification, ServerNotification},
};
use serde_json::{Value, json};
use std::{ffi::OsStr, fmt, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, RwLock, watch},
    task::JoinHandle,
    time::Instant,
};

pub(crate) const CHANNEL_CAPABILITY: &str = "claude/channel";
pub(crate) const CHANNEL_NOTIFICATION: &str = "notifications/claude/channel";
pub(crate) const CHANNEL_ENVIRONMENT: &str = "PSST_CLAUDE_CHANNEL";
const TURN_RECONCILE_WAIT: u8 = 10;
const MAX_TURN_OCCUPANCY: Duration = Duration::from_secs(5 * 60);
const DIAGNOSTIC_INTERVAL: Duration = Duration::from_millis(250);
const CHANNEL_CONTENT: &str = "Psst has durable pending mail. Use message_receive to inspect it; retrieval does not acknowledge. Process the pending work, then explicitly call message_acknowledge for each completed message.";
const BLOCKED_DIAGNOSTIC: &str = "psst-mcp: Claude Channel activation blocked; the notification was not safely completed. Check the installed Claude Channels preview, organization policy, and MCP connection before restarting this profile.";
const TRANSPORT_DIAGNOSTIC: &str = "psst-mcp: Claude Channel notification transport failed; activation is blocked to avoid a duplicate model turn.";

pub(crate) struct ClaudeChannelController {
    host: Arc<ClaudeChannelHost>,
    activation: Arc<ActivationRuntime>,
    diagnostic_stop: watch::Sender<bool>,
    diagnostic_task: Mutex<Option<JoinHandle<()>>>,
}

impl fmt::Debug for ClaudeChannelController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeChannelController")
            .finish_non_exhaustive()
    }
}

impl ClaudeChannelController {
    pub(crate) fn start(
        runtime: Arc<SessionRuntime>,
        profile: String,
        squad: String,
    ) -> Result<Arc<Self>, psst_application::ActivationContractError> {
        let host = Arc::new(ClaudeChannelHost {
            peer: RwLock::new(None),
            runtime: Arc::clone(&runtime),
        });
        let source: Arc<dyn ActivationSource> =
            Arc::new(SessionActivationSource::new(runtime, profile, squad)?);
        let activation = Arc::new(ActivationRuntime::start(
            source,
            Arc::clone(&host) as Arc<dyn ActivationHost>,
            ActivationPolicy::default(),
        )?);
        let (diagnostic_stop, diagnostic_rx) = watch::channel(false);
        let diagnostic_task =
            tokio::spawn(monitor_activation(Arc::clone(&activation), diagnostic_rx));
        Ok(Arc::new(Self {
            host,
            activation,
            diagnostic_stop,
            diagnostic_task: Mutex::new(Some(diagnostic_task)),
        }))
    }

    pub(crate) async fn connected(&self, peer: Peer<RoleServer>) {
        *self.host.peer.write().await = Some(peer);
    }

    pub(crate) async fn shutdown(&self) {
        self.activation.shutdown().await;
        let _ = self.diagnostic_stop.send(true);
        if let Some(task) = self.diagnostic_task.lock().await.take() {
            let _ = task.await;
        }
        self.host.peer.write().await.take();
    }
}

impl Drop for ClaudeChannelController {
    fn drop(&mut self) {
        let _ = self.diagnostic_stop.send(true);
        if let Ok(mut task) = self.diagnostic_task.try_lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }
}

async fn monitor_activation(activation: Arc<ActivationRuntime>, mut stop: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            biased;
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return;
                }
            }
            () = tokio::time::sleep(DIAGNOSTIC_INTERVAL) => {
                match activation.snapshot().await.phase {
                    ActivationPhase::Blocked => {
                        eprintln!("{BLOCKED_DIAGNOSTIC}");
                        return;
                    }
                    ActivationPhase::Stopped => return,
                    ActivationPhase::Quiet
                    | ActivationPhase::Pending
                    | ActivationPhase::Waking
                    | ActivationPhase::Running
                    | ActivationPhase::Backoff => {}
                }
            }
        }
    }
}

struct ClaudeChannelHost {
    peer: RwLock<Option<Peer<RoleServer>>>,
    runtime: Arc<SessionRuntime>,
}

impl ActivationHost for ClaudeChannelHost {
    fn start<'a>(
        &'a self,
        wake: &'a WakeMetadata,
    ) -> ActivationFuture<'a, Result<Box<dyn ActivationTurn>, HostFailure>> {
        Box::pin(async move {
            let peer = self
                .peer
                .read()
                .await
                .clone()
                .ok_or(HostFailure::RetryableBeforeStart)?;
            peer.send_notification(channel_notification(wake))
                .await
                .map_err(|_| {
                    eprintln!("{TRANSPORT_DIAGNOSTIC}");
                    HostFailure::OutcomeUnknown
                })?;
            Ok(Box::new(ClaudeChannelTurn {
                runtime: Arc::clone(&self.runtime),
                notified_oldest: wake.oldest_message_id().to_owned(),
            }) as Box<dyn ActivationTurn>)
        })
    }
}

struct ClaudeChannelTurn {
    runtime: Arc<SessionRuntime>,
    notified_oldest: String,
}

impl ActivationTurn for ClaudeChannelTurn {
    fn completed(self: Box<Self>) -> ActivationFuture<'static, Result<(), HostFailure>> {
        Box::pin(async move {
            let deadline = Instant::now() + MAX_TURN_OCCUPANCY;
            loop {
                if Instant::now() >= deadline {
                    return Err(HostFailure::OutcomeUnknown);
                }
                let inbox = self
                    .runtime
                    .inbox(1, TURN_RECONCILE_WAIT)
                    .await
                    .map_err(|_| HostFailure::OutcomeUnknown)?;
                if inbox.pending_count == 0
                    || inbox.oldest_message_id.as_deref() != Some(&self.notified_oldest)
                {
                    return Ok(());
                }
            }
        })
    }
}

pub(crate) fn channel_enabled_from_environment() -> Result<bool, ()> {
    parse_channel_enabled(std::env::var_os(CHANNEL_ENVIRONMENT).as_deref())
}

fn parse_channel_enabled(value: Option<&OsStr>) -> Result<bool, ()> {
    match value {
        None => Ok(false),
        Some(value) if value == OsStr::new("1") => Ok(true),
        Some(value) if value == OsStr::new("true") => Ok(true),
        Some(value) if value == OsStr::new("enabled") => Ok(true),
        Some(_) => Err(()),
    }
}

pub(crate) fn channel_notification(wake: &WakeMetadata) -> ServerNotification {
    ServerNotification::CustomNotification(CustomNotification::new(
        CHANNEL_NOTIFICATION,
        Some(channel_params(wake)),
    ))
}

fn channel_params(wake: &WakeMetadata) -> Value {
    json!({
        "content": CHANNEL_CONTENT,
        "meta": {
            "profile": wake.profile(),
            "squad": wake.squad(),
            "pending_count": wake.pending_count().to_string(),
            "highest_priority": match wake.highest_priority() {
                psst_protocol::MessagePriorityDto::Normal => "normal",
                psst_protocol::MessagePriorityDto::High => "high",
            },
            "oldest_message_id": wake.oldest_message_id(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use psst_protocol::MessagePriorityDto;
    use rmcp::{ServerHandler, ServiceExt};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[test]
    fn environment_opt_in_is_closed() {
        assert_eq!(parse_channel_enabled(None), Ok(false));
        for accepted in ["1", "true", "enabled"] {
            assert_eq!(parse_channel_enabled(Some(OsStr::new(accepted))), Ok(true));
        }
        for rejected in ["", "0", "false", "TRUE", " enabled", "enabled "] {
            assert_eq!(parse_channel_enabled(Some(OsStr::new(rejected))), Err(()));
        }
    }

    #[test]
    fn diagnostics_are_fixed_and_secret_free() {
        for diagnostic in [BLOCKED_DIAGNOSTIC, TRANSPORT_DIAGNOSTIC] {
            for forbidden in [
                "authorization",
                "Bearer",
                "resume_token",
                "message_body",
                "profile=",
                "squad=",
            ] {
                assert!(!diagnostic.contains(forbidden));
            }
        }
    }

    #[test]
    fn notification_is_fixed_bounded_metadata_without_participant_content() {
        let wake = WakeMetadata::new(
            "profile".into(),
            "squad".into(),
            7,
            MessagePriorityDto::High,
            "msg_oldest".into(),
        )
        .unwrap();
        let notification = channel_notification(&wake);
        let encoded = serde_json::to_value(notification).unwrap();
        assert_eq!(encoded["method"], CHANNEL_NOTIFICATION);
        assert_eq!(encoded["params"]["content"], CHANNEL_CONTENT);
        assert_eq!(encoded["params"]["meta"]["profile"], "profile");
        assert_eq!(encoded["params"]["meta"]["pending_count"], "7");
        assert_eq!(encoded["params"]["meta"]["highest_priority"], "high");
        let serialized = encoded.to_string();
        for forbidden in ["authorization", "Bearer", "resume_token", "message_body"] {
            assert!(!serialized.contains(forbidden));
        }
    }

    struct FakeChannelServer {
        wake: WakeMetadata,
    }

    impl ServerHandler for FakeChannelServer {
        async fn on_initialized(&self, context: rmcp::service::NotificationContext<RoleServer>) {
            let _ = context
                .peer
                .send_notification(channel_notification(&self.wake))
                .await;
        }
    }

    #[tokio::test]
    async fn fake_claude_receives_exact_channel_notification_over_mcp() {
        let wake = WakeMetadata::new(
            "profile".into(),
            "squad".into(),
            3,
            MessagePriorityDto::Normal,
            "msg_oldest".into(),
        )
        .unwrap();
        let (server_input, mut client_output) = tokio::io::duplex(4096);
        let (client_input, server_output) = tokio::io::duplex(4096);
        let interaction = tokio::spawn(async move {
            let mut client_input = BufReader::new(client_input);
            client_output
                .write_all(
                    br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fake-claude","version":"0"}}}
"#,
                )
                .await
                .unwrap();
            let mut line = String::new();
            client_input.read_line(&mut line).await.unwrap();
            assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 1);
            client_output
                .write_all(
                    br#"{"jsonrpc":"2.0","method":"notifications/initialized"}
"#,
                )
                .await
                .unwrap();
            line.clear();
            tokio::time::timeout(Duration::from_secs(2), client_input.read_line(&mut line))
                .await
                .expect("Channel notification arrives")
                .unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        });

        let service = FakeChannelServer { wake }
            .serve((server_input, server_output))
            .await
            .expect("fake Channel server initializes");
        let notification = interaction.await.expect("fake Claude joins");
        assert_eq!(notification["method"], CHANNEL_NOTIFICATION);
        let params = &notification["params"];
        assert_eq!(params["content"], CHANNEL_CONTENT);
        assert_eq!(params["meta"]["pending_count"], "3");
        assert_eq!(params["meta"]["oldest_message_id"], "msg_oldest");

        service.cancel().await.expect("fake server cancels cleanly");
    }
}
