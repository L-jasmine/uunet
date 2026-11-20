//! Signaling session for the uunet host role: Engine.IO v4 / Socket.IO v4
//! over WSS.
//!
//! Copied from omarchyuu `src/transport/signal/mod.rs` and trimmed to the
//! host path only: connect + room login, reconnect-key refresh, ping/pong,
//! and host event dispatch. All controller-side negotiation (offer, ICE
//! exchange, network switching) lives in omarchyuu and is intentionally
//! absent; the framing codec (`wire`) and TLS verification (`tls`) are
//! unmodified copies.

use crate::backend::nrd::RoomSession;
use anyhow::{Result, anyhow};

use serde_json::Value;
use std::collections::VecDeque;
use std::future::pending;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use transport::SignalTransport;
pub(crate) use transport::{SignalFailure, SocketState};
pub(crate) use negotiation::ControlIceServer;
pub use wire::{EnginePacket, SocketPacket, encode_event, encode_pong};
pub(crate) use wire::{encode_binary_event, gzip_sdp};

pub const AUTH_HEADER: &str = "X-NRD-AUTH";
pub const RECONNECT_HEADER: &str = "X-NRD-RECONN-KEY";
pub const CONTROLLING_HEADER: &str = "X-NRD-CONTROLLING";
const STREAMER_VERSION_HEADER: &str = "streamer_version";
const STREAMER_FLAG_HEADER: &str = "streamer_flag";
/// omarchyuu `protocol::official_version::SDK`.
const STREAMER_VERSION: &str = "V4.7.3";
const STREAMER_FLAG: &str = r#"{"sdp_flags":{"gzip_sdp":true}}"#;
const ROOM_INFO_ACK_ID: u64 = 1;
const ROOM_INFO_TIMEOUT: Duration = Duration::from_secs(10);
const RECONNECT_KEY_CHECK_INTERVAL: Duration = Duration::from_secs(30);
const RECONNECT_KEY_MAX_AGE: Duration = Duration::from_secs(300);
const RECONNECT_KEY_ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// SDP 上限(omarchyuu signal/mod.rs:42)。
pub(crate) const MAX_SDP_BYTES: u64 = 1024 * 1024;

pub(crate) mod host;
mod tls;
mod transport;
mod negotiation;
mod wire;

/// Installs the process-wide rustls crypto provider once. Idempotent; a
/// manually installed provider wins.
pub fn init_tls() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub struct SignalSession {
    transport: SignalTransport,
    room_info: Value,
    reconnect_key: Option<String>,
    last_reconnect_key_refresh: Option<Instant>,
    next_refresh_ack_id: u64,
    deferred: VecDeque<EnginePacket>,
}

// Owned by the socket worker, never by a cancellable caller's read future.
pub(crate) struct PendingBinaryEvent {
    namespace: String,
    id: Option<u64>,
    event: String,
    args: Vec<Value>,
    expected_attachments: usize,
    attachments: Vec<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalRole {
    Host,
    Controller,
}

pub(crate) type SignalPushHandler = std::sync::Arc<dyn Fn(&Value) + Send + Sync>;

impl SignalSession {
    pub(crate) fn socket_state(&self) -> tokio::sync::watch::Receiver<SocketState> {
        self.transport.state_receiver()
    }

    pub async fn connect(room: RoomSession, role: SignalRole) -> Result<Self> {
        Self::connect_cancellable(room, role, &tokio_util::sync::CancellationToken::new()).await
    }

    pub(crate) async fn connect_cancellable(
        room: RoomSession,
        role: SignalRole,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Self> {
        Self::connect_observed(room, role, cancel, None).await
    }

    pub(crate) async fn connect_observed(
        room: RoomSession,
        role: SignalRole,
        cancel: &tokio_util::sync::CancellationToken,
        push: Option<SignalPushHandler>,
    ) -> Result<Self> {
        room.validate()?;
        let connect_timeout = Duration::from_millis(room.ws_connect_timeout_ms.max(100) as u64);
        let mut session = Self {
            transport: SignalTransport::spawn(room, role, push),
            room_info: Value::Null,
            reconnect_key: None,
            last_reconnect_key_refresh: None,
            next_refresh_ack_id: 3,
            deferred: VecDeque::new(),
        };
        let result = tokio::select! {
            _ = cancel.cancelled() => Err(anyhow!("signaling connection cancelled")),
            result = async {
            timeout(connect_timeout, session.wait_namespace())
                .await.map_err(|_| SignalFailure::Socket("initial connection deadline expired (2009)".into()))??;
            timeout(ROOM_INFO_TIMEOUT, session.login_room())
                .await.map_err(|_| SignalFailure::Socket("room_info ACK timed out (2012)".into()))??;
            Ok(())
            } => result,
        };
        if let Err(error) = result {
            let _ = session.graceful_close().await;
            return Err(error);
        }
        tracing::debug!(?role, "signaling room login complete");
        Ok(session)
    }

    async fn wait_namespace(&mut self) -> Result<()> {
        loop {
            match self.transport.next_packet().await? {
                EnginePacket::Message(SocketPacket::Connect { namespace, .. })
                    if namespace == "/" =>
                {
                    return Ok(());
                }
                packet => self.deferred.push_back(packet),
            }
        }
    }

    async fn login_room(&mut self) -> Result<()> {
        self.send_text(encode_event("room_info", &[], Some(ROOM_INFO_ACK_ID))?)
            .await?;
        loop {
            match self.transport.next_packet().await? {
                EnginePacket::Message(SocketPacket::Ack { id, args, .. })
                    if id == ROOM_INFO_ACK_ID =>
                {
                    let info = args
                        .first()
                        .and_then(Value::as_object)
                        .filter(|info| !info.is_empty())
                        .ok_or_else(|| {
                            SignalFailure::Socket("room_info ACK is empty or invalid (2011)".into())
                        })?;
                    self.room_info = Value::Object(info.clone());
                    return Ok(());
                }
                packet => self.deferred.push_back(packet),
            }
        }
    }

    /// Host-side keep-alive: reconnect-key refresh, ping/pong, and host event
    /// dispatch. The controller path from omarchyuu is not carried over.
    pub(crate) async fn keep_alive_host(
        mut self,
        shutdown: oneshot::Receiver<()>,
        mut host: host::Session,
    ) -> Result<()> {
        let mut next_key_check = tokio::time::Instant::now() + RECONNECT_KEY_CHECK_INTERVAL;
        let mut pending_key_refresh: Option<(u64, tokio::time::Instant)> = None;
        let signal_result = tokio::select! {
            biased;
            _ = shutdown => Ok(()),
            result = async {
            loop {
            tokio::select! {
                host_event = host.next() => {
                    host.apply(host_event, &mut self).await?;
                }
                _ = tokio::time::sleep_until(next_key_check), if pending_key_refresh.is_none() => {
                    // UU's loop starts with an expired refresh age, checks
                    // after 30s, and waits another 30s after each operation
                    // completes. Header receipt is not a successful refresh.
                    if self.transport.is_connected() && self.last_reconnect_key_refresh.is_none_or(|last| last.elapsed() > RECONNECT_KEY_MAX_AGE) {
                        let ack_id = self.next_refresh_ack_id;
                        self.next_refresh_ack_id = ack_id.wrapping_add(1).max(3);
                        let refresh = encode_event(
                            "refresh_reconnect_key",
                            &[],
                            Some(ack_id),
                        )?;
                        if let Err(error) = self.send_text(refresh).await {
                            tracing::warn!(%error, "refresh_reconnect_key request failed; waiting for the socket state machine");
                        } else {
                            pending_key_refresh = Some((ack_id, tokio::time::Instant::now() + RECONNECT_KEY_ACK_TIMEOUT));
                            tracing::debug!(ack_id, "requested signaling reconnect key refresh");
                        }
                    }
                    next_key_check = tokio::time::Instant::now() + RECONNECT_KEY_CHECK_INTERVAL;
                }
                _ = async {
                    if let Some((_, deadline)) = pending_key_refresh {
                        tokio::time::sleep_until(deadline).await;
                    } else { pending::<()>().await; }
                } => {
                    tracing::warn!("refresh_reconnect_key ACK timed out; keeping the current signaling socket");
                    pending_key_refresh = None;
                    next_key_check = tokio::time::Instant::now() + RECONNECT_KEY_CHECK_INTERVAL;
                }
                packet = self.next_packet() => {
                    match packet {
                        Err(error) => break Err(error),
                        Ok(EnginePacket::Message(SocketPacket::Connect { .. })) => {
                            host.signaling_restored(&mut self).await?;
                        }
                        Ok(EnginePacket::Message(SocketPacket::Event { event, args, binary, .. })) => {
                            if let Err(error) = host.event(&event, &args, &binary, &mut self).await {
                                tracing::warn!(%error, "host signaling event rejected");
                            }
                        }
                        Ok(EnginePacket::Message(SocketPacket::Ack { id, args, .. }))
                            if pending_key_refresh.is_some_and(|(pending_id, _)| id == pending_id) =>
                        {
                            if args.len() < 2 || args[0].as_str() != Some("success") {
                                tracing::warn!("refresh_reconnect_key ACK was rejected");
                            } else {
                                if let Some(value) = args[1].get("reconnect_key") {
                                    self.reconnect_key = value.as_str().filter(|key| !key.is_empty()).map(str::to_owned);
                                }
                                // Official success may omit the field when
                                // an existing non-empty key remains valid.
                                if self.reconnect_key.is_some() {
                                    self.transport.set_reconnect_key(self.reconnect_key.clone());
                                    self.last_reconnect_key_refresh = Some(Instant::now());
                                    tracing::debug!(ack_id = id, "signaling reconnect key refreshed");
                                } else {
                                    tracing::warn!(ack_id = id, "refresh_reconnect_key ACK left an empty key");
                                }
                            }
                            pending_key_refresh = None;
                            next_key_check = tokio::time::Instant::now() + RECONNECT_KEY_CHECK_INTERVAL;
                        }
                        Ok(EnginePacket::Ping(payload)) => {
                            self.send_text(encode_pong(&payload)).await?;
                        }
                        Ok(EnginePacket::Close)
                        | Ok(EnginePacket::Message(SocketPacket::Disconnect { .. })) => {
                            break Err(SignalFailure::NamespaceClosed.into());
                        }
                        Ok(EnginePacket::Message(SocketPacket::ConnectError { .. })) => {
                            break Err(anyhow!("signaling server rejected the room session"));
                        }
                        _ => {}
                    }
                }
            }
        }} => result,
        };
        host.close().await;
        self.transport.set_controlling(false);
        let socket_result = self.graceful_close().await;
        signal_result.and(socket_result)
    }

    pub(crate) async fn close(mut self) -> Result<()> {
        self.graceful_close().await
    }

    async fn next_packet(&mut self) -> Result<EnginePacket> {
        if let Some(packet) = self.deferred.pop_front() {
            return Ok(packet);
        }
        self.transport.next_packet().await
    }

    async fn send_text(&mut self, value: impl Into<String>) -> Result<()> {
        self.transport
            .send(vec![Message::Text(value.into().into())])
    }

    pub(crate) fn send_binary_packet(&mut self, header: String, attachment: Vec<u8>) -> Result<()> {
        let mut packet = Vec::with_capacity(attachment.len() + 1);
        packet.push(0x04);
        packet.extend_from_slice(&attachment);
        self.transport.send(vec![
            Message::Text(header.into()),
            Message::Binary(packet.into()),
        ])
    }

    async fn graceful_close(&mut self) -> Result<()> {
        self.transport.close().await
    }
}
