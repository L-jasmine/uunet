//! 主控 Peer:发起 offer、创建官方五数据通道、承载端口转发 FILE 通道。
//!
//! 对应 omarchyuu `transport/rtc/peer.rs`(NativePeer)的端口转发子集:
//! 音视频 m-line 形状保持官方一致(audio_0 sendrecv + 5 条 recvonly
//! video),但无媒体收发;CONTROL 通道承载 PB ECHO 握手,固定纯 SCTP
//! 载体(offer 不写 `a=x-uuremote-mix-kcp`,KCP 载体留待后续)。

use crate::forward;
use crate::pb;
use crate::rtc::ConnectionCore;
use crate::rtc::negotiation::{register_uu_codecs, register_uu_header_extensions};
use anyhow::{Context, Result};
use std::collections::{HashSet, VecDeque};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::sync::CancellationToken;
use webrtc::api::media_engine::MediaEngine;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;
use webrtc::rtp_transceiver::RTCRtpTransceiverInit;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTPCodecType};
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::track::track_local::{TrackLocal, track_local_static_rtp::TrackLocalStaticRTP};

const PB_CONNECT_MAX_ATTEMPTS: u32 = 30;

/// 官方五通道优先级(omarchyuu channels.rs:559)。
fn official_data_channel_init(
    label: &str,
) -> webrtc::data_channel::data_channel_init::RTCDataChannelInit {
    use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
    use webrtc::data_channel::data_channel_init::RTCDataChannelPriority;
    let priority = match label {
        "CONTROL_DATA_CHANNEL" | "FILE_DATA_CHANNEL" => RTCDataChannelPriority::High,
        "TEXT_DATA_CHANNEL" | "STREAMER_DATA_CHANNEL" => RTCDataChannelPriority::Medium,
        _ => RTCDataChannelPriority::Low,
    };
    RTCDataChannelInit {
        ordered: Some(true),
        priority: Some(priority),
        ..Default::default()
    }
}

pub(crate) struct ControllerPeer {
    core: ConnectionCore,
    cancel: CancellationToken,
    connected: Arc<AtomicBool>,
    handshake_connected: Arc<AtomicBool>,
    transport: Arc<forward::Transport>,
    #[allow(dead_code)]
    control: Arc<RTCDataChannel>,
    remote_ice: AsyncMutex<RemoteIce>,
}

#[derive(Default)]
struct RemoteIce {
    current: HashSet<String>,
    pending: VecDeque<RTCIceCandidateInit>,
    retired: HashSet<String>,
}

impl ControllerPeer {
    /// 构造主控 Peer:注册编解码、添加官方 m-line 形状、创建五通道。
    pub(crate) async fn new(
        ice_servers: Vec<RTCIceServer>,
        relay: bool,
        cancel: CancellationToken,
    ) -> Result<(Self, mpsc::UnboundedReceiver<RTCIceCandidateInit>)> {
        let mut media = MediaEngine::default();
        register_uu_codecs(&mut media)?;
        register_uu_header_extensions(&mut media)?;
        let core = ConnectionCore::new(
            media,
            webrtc::interceptor::registry::Registry::new(),
            ConnectionCore::settings(),
            RTCConfiguration {
                ice_servers: ice_servers.clone(),
                ice_transport_policy: if relay {
                    RTCIceTransportPolicy::Relay
                } else {
                    RTCIceTransportPolicy::All
                },
                ..Default::default()
            },
        )
        .await?;
        let connection = core.connection.clone();
        let construction = cancel.clone().drop_guard();
        let (candidate_tx, candidates) = mpsc::unbounded_channel();
        connection.on_ice_candidate(Box::new(move |candidate| {
            if let Some(candidate) = candidate.and_then(|candidate| candidate.to_json().ok()) {
                let _ = candidate_tx.send(candidate);
            }
            Box::pin(async {})
        }));
        let connected = Arc::new(AtomicBool::new(false));
        let state_connected = connected.clone();
        let state_cancel = cancel.clone();
        connection.on_peer_connection_state_change(Box::new(move |state| {
            use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
            tracing::info!(?state, "controller peer connection state changed");
            state_connected.store(
                state == RTCPeerConnectionState::Connected,
                Ordering::Release,
            );
            if matches!(
                state,
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
            ) {
                state_cancel.cancel();
            }
            Box::pin(async {})
        }));
        // 官方主控形状:audio_0 sendrecv(Opus 发送器必须存在,否则受控端
        // 不认为是桌面控制器 profile)+ 五条 recvonly video m-line。
        let audio_track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: "audio/opus".into(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;stereo=1;useinbandfec=1".into(),
                ..Default::default()
            },
            "audio_0".into(),
            "audio_0".into(),
        ));
        connection
            .add_transceiver_from_track(
                audio_track.clone() as Arc<dyn TrackLocal + Send + Sync>,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Sendrecv,
                    send_encodings: Vec::new(),
                }),
            )
            .await
            .context("add audio_0 sendrecv transceiver")?;
        for index in 0..5 {
            connection
                .add_transceiver_from_kind(
                    RTPCodecType::Video,
                    Some(RTCRtpTransceiverInit {
                        direction: RTCRtpTransceiverDirection::Recvonly,
                        send_encodings: Vec::new(),
                    }),
                )
                .await
                .with_context(|| format!("add recvonly video transceiver {index}"))?;
        }

        let transport = Arc::new(forward::Transport::default());
        let handshake_connected = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicU32::new(0));
        // 主控是 offer 方:数据通道必须显式创建,否则 offer 没有
        // m=application 段(omarchyuu peer.rs:204-211)。先建后 offer。
        let mut channels = Vec::with_capacity(5);
        for label in [
            "CONTROL_DATA_CHANNEL",
            "TEXT_DATA_CHANNEL",
            "STREAMER_DATA_CHANNEL",
            "FILE_DATA_CHANNEL",
            "BINARY_DATA_CHANNEL",
        ] {
            let channel = connection
                .create_data_channel(label, Some(official_data_channel_init(label)))
                .await
                .with_context(|| format!("create {label}"))?;
            channels.push(channel);
        }
        let by_label = |name: &str| {
            channels
                .iter()
                .find(|c| c.label() == name)
                .cloned()
                .expect("官方通道标签必然存在")
        };
        let control = by_label("CONTROL_DATA_CHANNEL");
        let file = by_label("FILE_DATA_CHANNEL");
        let peer = Self {
            core,
            cancel: cancel.clone(),
            connected,
            handshake_connected: handshake_connected.clone(),
            transport: transport.clone(),
            control: control.clone(),
            remote_ice: AsyncMutex::new(RemoteIce::default()),
        };
        peer.wire_control(
            &control,
            handshake_connected.clone(),
            attempts,
            cancel.clone(),
        );
        peer.wire_file(&file, transport, cancel);
        construction.disarm();
        Ok((peer, candidates))
    }

    /// 直接接线 CONTROL(PB ECHO 握手)与 FILE(端口转发)通道。
    /// 通道由本端创建,不走 on_data_channel(那只会收到对端 DCEP 通道)。
    fn wire_control(
        &self,
        control: &Arc<RTCDataChannel>,
        handshake_connected: Arc<AtomicBool>,
        attempts: Arc<AtomicU32>,
        cancel: CancellationToken,
    ) {
        // ECHO 轮询 worker:立即一次,之后 500ms 一次,至多 30 次
        // (omarchyuu channels.rs:310 PB_CONNECT_MAX_ATTEMPTS)。
        let worker_connected = handshake_connected.clone();
        let worker_cancel = cancel.clone();
        let worker_attempts = attempts.clone();
        let worker_target = Arc::downgrade(control);
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(std::time::Duration::from_millis(500));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                if worker_cancel.is_cancelled() || worker_connected.load(Ordering::Acquire) {
                    return;
                }
                if worker_attempts.load(Ordering::Acquire) >= PB_CONNECT_MAX_ATTEMPTS {
                    tracing::warn!("controller PB handshake timed out");
                    return;
                }
                timer.tick().await;
                if worker_connected.load(Ordering::Acquire) {
                    return;
                }
                let attempt = worker_attempts.fetch_add(1, Ordering::AcqRel) + 1;
                let Some(channel) = worker_target.upgrade() else {
                    return;
                };
                let bytes = pb::bare_echo_request();
                match channel.send(&bytes::Bytes::from(bytes)).await {
                    Ok(_) => {
                        tracing::debug!(attempt, "controller sent official protobuf ECHO_REQUEST")
                    }
                    Err(error) => {
                        tracing::warn!(attempt, %error, "controller ECHO_REQUEST send failed")
                    }
                }
            }
        });
        let reply_handshake = handshake_connected.clone();
        let reply_target = Arc::downgrade(control);
        control.on_message(Box::new(move |message| {
            let reply_target = reply_target.clone();
            let handshake = reply_handshake.clone();
            let data = message.data;
            Box::pin(async move {
                match pb::receive(&data) {
                    Ok(received) => {
                        if received.handshake_complete {
                            handshake.store(true, Ordering::Release);
                            tracing::info!("controller PB handshake complete");
                        }
                        for response in received.messages {
                            if let Some(channel) = reply_target.upgrade() {
                                if let Err(error) =
                                    channel.send(&bytes::Bytes::from(response)).await
                                {
                                    tracing::warn!(%error, "controller PB response failed");
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "controller CONTROL message rejected")
                    }
                }
            })
        }));
    }

    fn wire_file(
        &self,
        file: &Arc<RTCDataChannel>,
        transport: Arc<forward::Transport>,
        cancel: CancellationToken,
    ) {
        transport.bind(file);
        let wake_transport = transport.clone();
        // 注册即生效,返回值无需使用。
        #[allow(unused_must_use)]
        file.on_buffered_amount_low(Box::new(move || {
            wake_transport.wake();
            Box::pin(async {})
        }));
        let message_transport = transport.clone();
        let message_cancel = cancel.clone();
        file.on_message(Box::new(move |message| {
            let transport = message_transport.clone();
            let cancelled = message_cancel.is_cancelled();
            let data = message.data;
            Box::pin(async move {
                if cancelled {
                    return;
                }
                if let Err(error) = transport.receive(&data) {
                    tracing::warn!(%error, "controller FILE message rejected");
                }
            })
        }));
    }

    /// 生成本地 offer(官方 m-line 形状,固定不带 mix-kcp:CONTROL 走纯 SCTP)。
    pub(crate) async fn offer(&self) -> Result<String> {
        let sdp = self.core.offer(false, "audio_0", None).await?;
        tracing::info!(sdp_bytes = sdp.len(), "controller created offer");
        Ok(sdp)
    }

    /// 安装被控 answer。
    pub(crate) async fn set_remote_answer(&self, sdp: String) -> Result<()> {
        let fragments: HashSet<String> = sdp
            .lines()
            .filter_map(|line| line.strip_prefix("a=ice-ufrag:"))
            .map(str::to_owned)
            .collect();
        let mixed = crate::rtc::negotiation::negotiated_mixed_kcp_version(&sdp)?;
        tracing::info!(
            sdp_bytes = sdp.len(),
            ?mixed,
            "controller installing remote answer"
        );
        let description =
            webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(sdp)?;
        let mut ice = self.remote_ice.lock().await;
        self.core
            .connection
            .set_remote_description(description)
            .await?;
        let previous = std::mem::replace(&mut ice.current, fragments);
        for fragment in previous {
            if !ice.current.contains(&fragment) {
                ice.retired.insert(fragment);
            }
        }
        let mut pending = std::mem::take(&mut ice.pending);
        while let Some(candidate) = pending.pop_front() {
            if candidate
                .username_fragment
                .as_ref()
                .is_none_or(|fragment| ice.current.contains(fragment))
            {
                if let Err(error) = self.core.connection.add_ice_candidate(candidate).await {
                    tracing::debug!(%error, "discarded queued controller ICE candidate");
                }
            } else if candidate
                .username_fragment
                .as_ref()
                .is_some_and(|fragment| !ice.retired.contains(fragment))
            {
                ice.pending.push_back(candidate);
            }
        }
        Ok(())
    }

    pub(crate) async fn add_remote_candidate(
        &self,
        mut candidate: RTCIceCandidateInit,
    ) -> Result<()> {
        if self.ended() {
            return Ok(());
        }
        if candidate
            .username_fragment
            .as_ref()
            .is_none_or(String::is_empty)
        {
            candidate.username_fragment = ufrag_of(&candidate.candidate);
        }
        let mut ice = self.remote_ice.lock().await;
        if candidate
            .username_fragment
            .as_ref()
            .is_some_and(|fragment| ice.retired.contains(fragment))
        {
            return Ok(());
        }
        if ice.current.is_empty()
            || candidate
                .username_fragment
                .as_ref()
                .is_some_and(|fragment| !ice.current.contains(fragment))
        {
            anyhow::ensure!(ice.pending.len() < 256, "等待SDP的ICE候选过多");
            ice.pending.push_back(candidate);
            return Ok(());
        }
        self.core.connection.add_ice_candidate(candidate).await?;
        Ok(())
    }

    /// 本地候选 → soac 上行载荷所需的规范化(补 sdp_mid)。
    pub(crate) async fn local_candidate(
        &self,
        candidate: RTCIceCandidateInit,
    ) -> Option<RTCIceCandidateInit> {
        if self.ended() {
            return None;
        }
        let mut candidate = candidate;
        if candidate.candidate.is_empty() {
            return None;
        }
        if candidate.sdp_mid.as_ref().is_none_or(String::is_empty) {
            let index = usize::from(candidate.sdp_mline_index.unwrap_or(0));
            candidate.sdp_mid = match self.core.connection.local_description().await {
                Some(description) => description
                    .sdp
                    .split("m=")
                    .skip(1)
                    .nth(index)
                    .and_then(|media| media.lines().find_map(|line| line.strip_prefix("a=mid:")))
                    .map(str::to_owned),
                None => return None,
            };
        }
        Some(candidate)
    }

    pub(crate) fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub(crate) fn handshake_connected(&self) -> bool {
        self.handshake_connected.load(Ordering::Acquire)
    }

    pub(crate) fn file_ready(&self) -> bool {
        self.transport.ready()
    }

    pub(crate) fn transport(&self) -> Arc<forward::Transport> {
        self.transport.clone()
    }

    pub(crate) fn ended(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// 会话结束由信令侧统一关闭;保留显式关闭入口。
    #[allow(dead_code)]
    pub(crate) async fn close(&self) -> Result<()> {
        self.cancel.cancel();
        self.transport.close();
        self.core.close().await
    }
}

fn ufrag_of(candidate: &str) -> Option<String> {
    candidate
        .split_ascii_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|pair| (pair[0] == "ufrag").then(|| pair[1].to_string()))
}
