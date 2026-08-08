use psst_application::{
    AgentStatusInput, AgentStatusOutput, Availability, ConfigFlags, ConfigInputs, ConfigResolver,
    EmptyInput, LocalErrorCode, McpSafeError, MessageAcknowledgeInput, MessageAcknowledgeOutput,
    MessageReceiveInput, MessageReceiveOutput, MessageSendInput, MessageSendOutput, MessageView,
    PlatformPaths, Priority, ProfilePaths, RosterMemberView, RuntimeSpec, SecurityNotice,
    SessionError, SessionHealth, SessionRuntime, SessionView, SquadDescribeInput,
    SquadDescribeOutput, SquadJoinInput, SquadJoinOutput, SquadLeaveOutput, SquadListOutput,
    SquadRosterOutput, SquadView, TrustLabel, UnboundRuntimeSpec, UntrustedPriority, UntrustedText,
    canonical_tool_text, load_profile, map_client_error, verify_profile_origin,
};
use psst_client::{Client, ClientConfig};
use psst_protocol::{
    AckMessagesRequest, AgentModeDto, AvailabilityDto, ClientMetadata, JoinSquadRequest,
    MessageDto, MessagePriorityDto, SquadSummary,
};
use serde::Serialize;
use serde_json::{Map, Value};
use std::{collections::BTreeMap, io, sync::Arc, time::Duration};
use tokio::sync::{Notify, RwLock};

pub(crate) struct DispatchState {
    client: Arc<Client>,
    profile: String,
    relay_origin: String,
    paths: ProfilePaths,
    runtime: RwLock<RuntimeSlot>,
    transition_done: Notify,
    #[cfg(test)]
    publication_probe: Option<Arc<PublicationProbe>>,
}

#[cfg(test)]
struct PublicationProbe {
    ready: Notify,
    release: Notify,
}

enum RuntimeSlot {
    Unbound,
    Transition,
    Bound(Arc<SessionRuntime>),
}

impl std::fmt::Debug for DispatchState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DispatchState")
            .field("profile", &self.profile)
            .field("relay_origin", &self.relay_origin)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct ToolFailure(pub LocalErrorCode);

impl DispatchState {
    pub(crate) async fn from_environment() -> Result<Arc<Self>, LocalErrorCode> {
        let platform = PlatformPaths::detect().map_err(|_| LocalErrorCode::InvalidConfiguration)?;
        let environment = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect::<BTreeMap<_, _>>();
        let resolved = ConfigResolver::new(platform)
            .resolve(&ConfigInputs {
                flags: ConfigFlags::default(),
                environment,
            })
            .map_err(|_| LocalErrorCode::InvalidConfiguration)?;
        let paths = ProfilePaths::for_profile(
            &resolved.paths,
            &resolved.relay_origin.value,
            &resolved.profile.value,
        )
        .map_err(|_| LocalErrorCode::InvalidConfiguration)?;
        let client = Arc::new(
            Client::new(&resolved.relay_origin.value, ClientConfig::default())
                .map_err(|error| map_client_error(&error))?,
        );
        SessionRuntime::recover_orphaned_leave(
            paths.clone(),
            resolved.relay_origin.value.clone(),
            resolved.profile.value.clone(),
        )
        .await
        .map_err(|error| map_session_error(&error))?;
        let binding = load_profile(&paths.metadata).map_err(|error| local_io(&error))?;
        let runtime = if let Some(binding) = binding {
            verify_profile_origin(&binding, &resolved.relay_origin.value)
                .map_err(|_| LocalErrorCode::ProfileOriginMismatch)?;
            RuntimeSlot::Bound(Arc::new(
                SessionRuntime::start(
                    Arc::clone(&client),
                    RuntimeSpec {
                        profile: binding,
                        paths: paths.clone(),
                        mode: AgentModeDto::Cooperative,
                        client_metadata: mcp_metadata(),
                        shutdown_bound: Duration::from_secs(5),
                    },
                )
                .await
                .map_err(|error| map_session_error(&error))?,
            ))
        } else {
            RuntimeSlot::Unbound
        };
        Ok(Arc::new(Self {
            client,
            profile: resolved.profile.value,
            relay_origin: resolved.relay_origin.value,
            paths,
            runtime: RwLock::new(runtime),
            transition_done: Notify::new(),
            #[cfg(test)]
            publication_probe: None,
        }))
    }

    pub(crate) async fn call(
        self: &Arc<Self>,
        name: &str,
        arguments: Map<String, Value>,
    ) -> Result<(Value, String), ToolFailure> {
        let output = match name {
            "squad_join" => self.join(decode(arguments)?).await?,
            "squad_leave" => {
                let _: EmptyInput = decode(arguments)?;
                self.leave().await?
            }
            "squad_list" => {
                let _: EmptyInput = decode(arguments)?;
                value(SquadListOutput {
                    security_notice: SecurityNotice::ParticipantContentIsUntrusted,
                    squads: self
                        .client
                        .list_squads()
                        .await
                        .map_err(|error| client_failure(&error))?
                        .into_iter()
                        .map(squad_view)
                        .collect(),
                })?
            }
            "squad_describe" => {
                let input: SquadDescribeInput = decode(arguments)?;
                value(SquadDescribeOutput {
                    security_notice: SecurityNotice::ParticipantContentIsUntrusted,
                    squad: squad_view(
                        self.client
                            .describe_squad(&input.squad)
                            .await
                            .map_err(|error| client_failure(&error))?,
                    ),
                })?
            }
            "squad_roster" => {
                let _: EmptyInput = decode(arguments)?;
                let runtime = self.active().await?;
                let roster = runtime
                    .roster()
                    .await
                    .map_err(|error| session_failure(&error))?;
                value(SquadRosterOutput {
                    security_notice: SecurityNotice::ParticipantContentIsUntrusted,
                    squad: UntrustedText::participant(roster.squad),
                    members: roster
                        .members
                        .into_iter()
                        .map(|member| RosterMemberView {
                            membership_id: member.membership_id,
                            name: UntrustedText::participant(member.name),
                            role: UntrustedText::participant(member.role),
                            membership_state: enum_text(member.membership_state),
                            presence: enum_text(member.presence),
                            availability: UntrustedText::participant(enum_text(
                                member.availability,
                            )),
                            availability_source: UntrustedText::participant(enum_text(
                                member.availability_source,
                            )),
                            availability_observed_at: member.availability_observed_at.to_string(),
                            mode: member
                                .mode
                                .map(|mode| UntrustedText::participant(enum_text(mode))),
                            last_seen_at: member.last_seen_at.map(|time| time.to_string()),
                        })
                        .collect(),
                })?
            }
            "message_send" => self.send(decode(arguments)?).await?,
            "message_receive" => self.receive(decode(arguments)?).await?,
            "message_acknowledge" => self.acknowledge(decode(arguments)?).await?,
            "agent_status" => self.status(decode(arguments)?).await?,
            _ => return Err(ToolFailure(LocalErrorCode::InvalidInput)),
        };
        let text =
            canonical_tool_text(&output).map_err(|_| ToolFailure(LocalErrorCode::Internal))?;
        Ok((output, text))
    }

    async fn join(self: &Arc<Self>, input: SquadJoinInput) -> Result<Value, ToolFailure> {
        {
            let mut slot = self.runtime.write().await;
            match &*slot {
                RuntimeSlot::Unbound => *slot = RuntimeSlot::Transition,
                RuntimeSlot::Bound(_) => {
                    return Err(ToolFailure(LocalErrorCode::ProfileAlreadyBound));
                }
                RuntimeSlot::Transition => return Err(ToolFailure(LocalErrorCode::LocalLock)),
            }
        }
        let state = Arc::clone(self);
        tokio::spawn(async move { state.join_owned(input).await })
            .await
            .map_err(|_| ToolFailure(LocalErrorCode::Internal))?
    }

    async fn join_owned(&self, input: SquadJoinInput) -> Result<Value, ToolFailure> {
        let joined = SessionRuntime::join_and_bind(
            Arc::clone(&self.client),
            UnboundRuntimeSpec {
                relay_origin: self.relay_origin.clone(),
                profile_name: self.profile.clone(),
                squad: input.squad,
                paths: self.paths.clone(),
                shutdown_bound: Duration::from_secs(5),
            },
            JoinSquadRequest {
                name: input.name,
                role: input.role,
                mode: AgentModeDto::Cooperative,
                client: mcp_metadata(),
                mission: input.mission,
            },
        )
        .await;
        let joined = match joined {
            Ok(joined) => joined,
            Err(error) => {
                *self.runtime.write().await = RuntimeSlot::Unbound;
                self.transition_done.notify_waiters();
                return Err(session_failure(&error));
            }
        };
        #[cfg(test)]
        if let Some(probe) = &self.publication_probe {
            probe.ready.notify_one();
            probe.release.notified().await;
        }
        let response = joined.response;
        *self.runtime.write().await = RuntimeSlot::Bound(Arc::new(joined.runtime));
        self.transition_done.notify_waiters();
        value(SquadJoinOutput {
            session: session_view(response),
        })
    }

    async fn leave(self: &Arc<Self>) -> Result<Value, ToolFailure> {
        let active = {
            let mut slot = self.runtime.write().await;
            match std::mem::replace(&mut *slot, RuntimeSlot::Transition) {
                RuntimeSlot::Bound(runtime) => runtime,
                RuntimeSlot::Unbound => {
                    *slot = RuntimeSlot::Unbound;
                    return Err(ToolFailure(LocalErrorCode::ProfileUnbound));
                }
                RuntimeSlot::Transition => return Err(ToolFailure(LocalErrorCode::LocalLock)),
            }
        };
        let state = Arc::clone(self);
        tokio::spawn(async move { state.leave_owned(active).await })
            .await
            .map_err(|_| ToolFailure(LocalErrorCode::Internal))?
    }

    async fn leave_owned(&self, active: Arc<SessionRuntime>) -> Result<Value, ToolFailure> {
        let response = match active.leave().await {
            Ok(response) => response,
            Err(error) => {
                *self.runtime.write().await = RuntimeSlot::Bound(active);
                self.transition_done.notify_waiters();
                return Err(session_failure(&error));
            }
        };
        #[cfg(test)]
        if let Some(probe) = &self.publication_probe {
            probe.ready.notify_one();
            probe.release.notified().await;
        }
        *self.runtime.write().await = RuntimeSlot::Unbound;
        self.transition_done.notify_waiters();
        value(SquadLeaveOutput {
            left: true,
            left_at: response.left_at.to_string(),
        })
    }

    async fn send(&self, input: MessageSendInput) -> Result<Value, ToolFailure> {
        let priority = match input.priority {
            Priority::Normal => MessagePriorityDto::Normal,
            Priority::High => MessagePriorityDto::High,
        };
        let prepared = self
            .client
            .prepare_send(
                input.recipient,
                input.body,
                priority,
                input.reply_to,
                input.correlation_id,
            )
            .map_err(|error| client_failure(&error))?;
        let runtime = self.active().await?;
        let response = runtime
            .send_prepared(&prepared)
            .await
            .map_err(|error| session_failure(&error))?;
        value(MessageSendOutput {
            security_notice: SecurityNotice::ParticipantContentIsUntrusted,
            message: message_view(response.message),
            idempotent_replay: response.idempotent_replay,
        })
    }

    async fn receive(&self, input: MessageReceiveInput) -> Result<Value, ToolFailure> {
        let runtime = self.active().await?;
        let acknowledged_ids = if input.acknowledge_ids.is_empty() {
            Vec::new()
        } else {
            runtime
                .acknowledge(&AckMessagesRequest {
                    message_ids: input.acknowledge_ids,
                })
                .await
                .map_err(|error| session_failure(&error))?
                .acknowledged_ids
        };
        let inbox = runtime
            .inbox(input.limit, input.wait_seconds)
            .await
            .map_err(|error| session_failure(&error))?;
        value(MessageReceiveOutput {
            security_notice: SecurityNotice::ParticipantContentIsUntrusted,
            acknowledged_ids,
            pending_count: inbox.pending_count,
            messages: inbox.messages.into_iter().map(message_view).collect(),
        })
    }

    async fn acknowledge(&self, input: MessageAcknowledgeInput) -> Result<Value, ToolFailure> {
        let runtime = self.active().await?;
        let response = runtime
            .acknowledge(&AckMessagesRequest {
                message_ids: input.message_ids,
            })
            .await
            .map_err(|error| session_failure(&error))?;
        value(MessageAcknowledgeOutput {
            acknowledged_ids: response.acknowledged_ids,
        })
    }

    async fn status(&self, input: AgentStatusInput) -> Result<Value, ToolFailure> {
        let runtime = {
            let slot = self.runtime.read().await;
            match &*slot {
                RuntimeSlot::Bound(runtime) => Some(Arc::clone(runtime)),
                RuntimeSlot::Unbound => None,
                RuntimeSlot::Transition => return Err(ToolFailure(LocalErrorCode::LocalLock)),
            }
        };
        let Some(runtime) = runtime else {
            return value(AgentStatusOutput {
                profile: self.profile.clone(),
                connected: false,
                degraded: false,
                availability: Availability::Unknown,
                lease_expires_at: None,
                heartbeat_interval_seconds: None,
            });
        };
        if let Some(availability) = input.availability {
            runtime
                .report_availability(to_protocol_availability(availability))
                .await
                .map_err(|error| session_failure(&error))?;
        }
        let snapshot = runtime.snapshot().await;
        value(AgentStatusOutput {
            profile: self.profile.clone(),
            connected: snapshot.health == SessionHealth::Ready,
            degraded: snapshot.health != SessionHealth::Ready,
            availability: from_protocol_availability(snapshot.availability),
            lease_expires_at: Some(snapshot.lease_expires_at.to_string()),
            heartbeat_interval_seconds: Some(snapshot.heartbeat_interval_seconds),
        })
    }

    pub(crate) async fn shutdown(&self) -> Result<(), ToolFailure> {
        let runtime = loop {
            let settled = self.transition_done.notified();
            let mut slot = self.runtime.write().await;
            match std::mem::replace(&mut *slot, RuntimeSlot::Transition) {
                RuntimeSlot::Bound(runtime) => break Some(runtime),
                RuntimeSlot::Unbound => break None,
                RuntimeSlot::Transition => {
                    drop(slot);
                    tokio::time::timeout(Duration::from_secs(5), settled)
                        .await
                        .map_err(|_| ToolFailure(LocalErrorCode::OutcomeUnknown))?;
                }
            }
        };
        let result = if let Some(runtime) = runtime {
            runtime
                .shutdown()
                .await
                .map_err(|error| session_failure(&error))
        } else {
            Ok(())
        };
        *self.runtime.write().await = RuntimeSlot::Unbound;
        result
    }

    async fn active(&self) -> Result<Arc<SessionRuntime>, ToolFailure> {
        let slot = self.runtime.read().await;
        match &*slot {
            RuntimeSlot::Bound(runtime) => Ok(Arc::clone(runtime)),
            RuntimeSlot::Unbound => Err(ToolFailure(LocalErrorCode::ProfileUnbound)),
            RuntimeSlot::Transition => Err(ToolFailure(LocalErrorCode::LocalLock)),
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(arguments: Map<String, Value>) -> Result<T, ToolFailure> {
    serde_json::from_value(Value::Object(arguments))
        .map_err(|_| ToolFailure(LocalErrorCode::InvalidInput))
}
fn value<T: Serialize>(value: T) -> Result<Value, ToolFailure> {
    serde_json::to_value(value).map_err(|_| ToolFailure(LocalErrorCode::Internal))
}
fn client_failure(error: &psst_client::Error) -> ToolFailure {
    ToolFailure(map_client_error(error))
}
fn session_failure(error: &SessionError) -> ToolFailure {
    ToolFailure(map_session_error(error))
}
fn local_io(error: &io::Error) -> LocalErrorCode {
    match error.kind() {
        io::ErrorKind::PermissionDenied => LocalErrorCode::LocalPermission,
        io::ErrorKind::WouldBlock | io::ErrorKind::AddrInUse => LocalErrorCode::ProfileLocked,
        _ => LocalErrorCode::LocalRead,
    }
}
fn map_session_error(error: &SessionError) -> LocalErrorCode {
    match error {
        SessionError::Local(error) => local_io(error),
        SessionError::Relay(error) => map_client_error(error),
        SessionError::ShutdownTimedOut | SessionError::RecoveryOutcomeUnknown => {
            LocalErrorCode::OutcomeUnknown
        }
        SessionError::NotReady => LocalErrorCode::InvalidSession,
        SessionError::Unbound => LocalErrorCode::ProfileUnbound,
        SessionError::SendCapacity | SessionError::OperationCapacity => LocalErrorCode::LocalLock,
    }
}
fn mcp_metadata() -> ClientMetadata {
    ClientMetadata {
        kind: "mcp".into(),
        hostname: None,
        version: Some(env!("CARGO_PKG_VERSION").into()),
    }
}
fn squad_view(squad: SquadSummary) -> SquadView {
    SquadView {
        id: squad.id,
        name: UntrustedText::participant(squad.name),
        mission: UntrustedText::participant(squad.mission),
        state: enum_text(squad.state),
        created_at: squad.created_at.to_string(),
        archived_at: squad.archived_at.map(|time| time.to_string()),
    }
}
fn session_view(session: psst_protocol::SessionResponse) -> SessionView {
    SessionView {
        squad: squad_view(session.squad),
        member_name: UntrustedText::participant(session.member_name),
        role: UntrustedText::participant(session.role),
        heartbeat_interval_seconds: session.heartbeat_interval_seconds,
        lease_seconds: session.lease_seconds,
        lease_expires_at: session.lease_expires_at.to_string(),
    }
}
fn message_view(message: MessageDto) -> MessageView {
    MessageView {
        trust: TrustLabel::UntrustedParticipantContent,
        sequence: message.sequence.value(),
        id: message.id,
        squad: UntrustedText::participant(message.squad),
        sender: UntrustedText::participant(message.sender),
        recipient: UntrustedText::participant(message.recipient),
        untrusted_body: message.body,
        priority: UntrustedPriority {
            trust: TrustLabel::UntrustedParticipantContent,
            value: match message.priority {
                MessagePriorityDto::Normal => Priority::Normal,
                MessagePriorityDto::High => Priority::High,
            },
        },
        reply_to: message.reply_to.map(UntrustedText::participant),
        correlation_id: message.correlation_id.map(UntrustedText::participant),
        created_at: message.created_at.to_string(),
        acknowledged_at: message.acknowledged_at.map(|time| time.to_string()),
    }
}
fn enum_text<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}
const fn to_protocol_availability(value: Availability) -> AvailabilityDto {
    match value {
        Availability::Idle => AvailabilityDto::Idle,
        Availability::Busy => AvailabilityDto::Busy,
        Availability::Blocked => AvailabilityDto::Blocked,
        Availability::Unknown => AvailabilityDto::Unknown,
    }
}
const fn from_protocol_availability(value: AvailabilityDto) -> Availability {
    match value {
        AvailabilityDto::Idle => Availability::Idle,
        AvailabilityDto::Busy => Availability::Busy,
        AvailabilityDto::Blocked => Availability::Blocked,
        AvailabilityDto::Unknown => Availability::Unknown,
    }
}

pub(crate) fn error_output(code: LocalErrorCode) -> (Value, String) {
    let output = psst_application::McpErrorOutput {
        error: McpSafeError::from(code),
    };
    let value = serde_json::to_value(&output).unwrap_or_else(|_| serde_json::json!({"error":{"code":"internal","message":"An internal error occurred.","retryable":false}}));
    let text = canonical_tool_text(&output).unwrap_or_else(|_| "{\"error\":{\"code\":\"internal\",\"message\":\"An internal error occurred.\",\"retryable\":false}}".into());
    (value, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use psst_protocol::CreateSquadRequest;
    use std::net::{Ipv4Addr, TcpListener};
    use tokio::sync::{oneshot, watch};

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dropped_join_and_leave_waiters_cannot_strand_transition_or_shutdown() {
        let temp = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let origin = format!("http://{address}");
        let mut relay_config = psst_relay::RelayConfig::local(temp.path().join("relay.db"));
        relay_config.bind = address;
        let (relay_shutdown, relay_shutdown_rx) = watch::channel(false);
        let (startup_tx, startup_rx) = oneshot::channel();
        let relay = tokio::spawn(psst_relay::serve_with_startup(
            relay_config,
            relay_shutdown_rx,
            startup_tx,
        ));
        startup_rx.await.unwrap();
        let client = Arc::new(Client::new(&origin, ClientConfig::default()).unwrap());
        client
            .create_squad(&CreateSquadRequest {
                name: "owned-transition".into(),
                mission: "cancellation ownership".into(),
            })
            .await
            .unwrap();
        let platform = PlatformPaths {
            config_dir: temp.path().join("config"),
            data_dir: temp.path().join("data"),
            runtime_dir: temp.path().join("runtime"),
        };
        let paths = ProfilePaths::for_profile(&platform, &origin, "owned").unwrap();

        let join_probe = Arc::new(PublicationProbe {
            ready: Notify::new(),
            release: Notify::new(),
        });
        let state = test_state(
            Arc::clone(&client),
            origin.clone(),
            paths.clone(),
            RuntimeSlot::Unbound,
            Arc::clone(&join_probe),
        );
        let join_state = Arc::clone(&state);
        let join_waiter = tokio::spawn(async move {
            join_state
                .join(SquadJoinInput {
                    squad: "owned-transition".into(),
                    name: "owned-worker".into(),
                    role: "test".into(),
                    mission: None,
                })
                .await
        });
        join_probe.ready.notified().await;
        join_waiter.abort();
        let _ = join_waiter.await;
        release_transition_into_shutdown(state, join_probe).await;

        let binding = psst_application::load_profile(&paths.metadata)
            .unwrap()
            .expect("owned join must publish durable binding before shutdown");
        let runtime = SessionRuntime::start(
            Arc::clone(&client),
            RuntimeSpec {
                profile: binding,
                paths: paths.clone(),
                mode: AgentModeDto::Cooperative,
                client_metadata: mcp_metadata(),
                shutdown_bound: Duration::from_secs(5),
            },
        )
        .await
        .unwrap();
        let leave_probe = Arc::new(PublicationProbe {
            ready: Notify::new(),
            release: Notify::new(),
        });
        let state = test_state(
            client,
            origin,
            paths.clone(),
            RuntimeSlot::Bound(Arc::new(runtime)),
            Arc::clone(&leave_probe),
        );
        let leave_state = Arc::clone(&state);
        let leave_waiter = tokio::spawn(async move { leave_state.leave().await });
        leave_probe.ready.notified().await;
        leave_waiter.abort();
        let _ = leave_waiter.await;
        release_transition_into_shutdown(state, leave_probe).await;
        assert!(
            psst_application::load_profile(&paths.metadata)
                .unwrap()
                .is_none()
        );

        relay_shutdown.send(true).unwrap();
        relay.await.unwrap().unwrap();
    }

    async fn release_transition_into_shutdown(
        state: Arc<DispatchState>,
        probe: Arc<PublicationProbe>,
    ) {
        let mut shutdown = tokio::spawn(async move { state.shutdown().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut shutdown)
                .await
                .is_err()
        );
        probe.release.notify_one();
        shutdown.await.unwrap().unwrap();
    }

    fn test_state(
        client: Arc<Client>,
        relay_origin: String,
        paths: ProfilePaths,
        runtime: RuntimeSlot,
        publication_probe: Arc<PublicationProbe>,
    ) -> Arc<DispatchState> {
        Arc::new(DispatchState {
            client,
            profile: "owned".into(),
            relay_origin,
            paths,
            runtime: RwLock::new(runtime),
            transition_done: Notify::new(),
            publication_probe: Some(publication_probe),
        })
    }
}
