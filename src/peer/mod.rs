//! 被控 Peer:WebRTC 连接所有权 + 数据通道绑定(uunet 裁剪版)。
//!
//! 对应 omarchyuu `features/host/peer/mod.rs` 的 data-only 路径:全量注册
//! 编解码保证 answer 与 offer 的 m-line 相容,但零采集零媒体发送;
//! FILE 通道挂端口转发(H2),CONTROL 通道挂 PB 握手与 KCP(H2 起)。

pub(crate) mod controller;
pub(crate) mod handshake;

#[allow(unused_imports)]
use crate::kcp::{ControlReceiver, UuKcpControl};
use crate::pb;
use crate::rtc::ConnectionCore;
use anyhow::{Context, Result};
use std::collections::{HashSet, VecDeque};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::sync::CancellationToken;
use webrtc::api::media_engine::{MIME_TYPE_H264, MIME_TYPE_HEVC, MIME_TYPE_OPUS, MediaEngine};
#[allow(unused_imports)]
use webrtc::data_channel::{RTCDataChannel, data_channel_state::RTCDataChannelState};
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTCRtpHeaderExtensionCapability, RTPCodecType,
};
use webrtc::track::track_local::{TrackLocal, track_local_static_rtp::TrackLocalStaticRTP};

pub(crate) struct Peer {
    core: ConnectionCore,
    cancel: CancellationToken,
    #[allow(dead_code)]
    connected: Arc<AtomicBool>,
    #[allow(dead_code)]
    kcp: UuKcpControl,
    handshake: handshake::HostHandshake,
    control_channel: Arc<std::sync::Mutex<Option<std::sync::Weak<RTCDataChannel>>>>,
    remote_ice: AsyncMutex<RemoteIce>,
    ports: crate::forward::host::Receiver,
}

#[derive(Default)]
struct RemoteIce {
    /// 当前远端 SDP 里的 ICE 代次(ufrag)。
    current: HashSet<String>,
    /// 早于 offer 到达、或属于已退役代次的候选。
    pending: VecDeque<RTCIceCandidateInit>,
    retired: HashSet<String>,
}

impl Peer {
    /// 构造被控 Peer:注册编解码 + 建连 + 绑定通道回调;
    /// 同时交出本地候选通道(Session 据此发 soac candidate)。
    pub(crate) async fn new(
        ice_servers: Vec<RTCIceServer>,
        relay: bool,
        cancel: CancellationToken,
    ) -> Result<(
        Self,
        mpsc::UnboundedReceiver<webrtc::ice_transport::ice_candidate::RTCIceCandidateInit>,
    )> {
        let mut media = MediaEngine::default();
        register_codecs(&mut media)?;
        // 数据型会话不发媒体;空 Registry 即可。
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
        // audio_0:answer 里音频 m-line 需要一个可发送方向;从不真正推帧。
        let audio_track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.into(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;stereo=1;useinbandfec=1".into(),
                ..Default::default()
            },
            "audio_0".into(),
            "audio_0".into(),
        ));
        connection
            .add_track(audio_track as Arc<dyn TrackLocal + Send + Sync>)
            .await
            .context("add host audio track")?;

        let state_connected = connected.clone();
        let state_cancel = cancel.clone();
        connection.on_peer_connection_state_change(Box::new(move |state| {
            tracing::info!(?state, "host peer connection state changed");
            let connected = state == webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connected;
            state_connected.store(connected, Ordering::Release);
            if matches!(
                state,
                webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed
                    | webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed
            ) {
                state_cancel.cancel();
            }
            Box::pin(async {})
        }));

        let kcp = UuKcpControl::default();
        // 端口转发:FILE 通道的接收器 + 拨号 actor(生命周期随 Peer)。
        let lease = crate::host::access::Lease::new();
        let (ports, ports_input) = crate::forward::host::Receiver::new(lease, connected.clone());
        tokio::spawn(crate::forward::host::run(
            ports.clone(),
            ports_input,
            cancel.clone(),
        ));
        let peer = Self {
            core,
            cancel,
            connected,
            kcp,
            handshake: handshake::HostHandshake::default(),
            control_channel: Arc::new(std::sync::Mutex::new(None)),
            remote_ice: AsyncMutex::new(RemoteIce::default()),
            ports,
        };
        let ports_for_bind = peer.ports.clone();
        peer.bind_channels(connection, ports_for_bind)?;
        // 成功路径 disarm:构造失败时 guard 的 drop 才负责取消角色任务。
        construction.disarm();
        Ok((peer, candidates))
    }

    /// 按标签绑定数据通道:CONTROL→PB 握手(+H2 KCP),FILE→端口转发(H2)。
    fn bind_channels(
        &self,
        connection: Arc<webrtc::peer_connection::RTCPeerConnection>,
        ports: crate::forward::host::Receiver,
    ) -> Result<()> {
        let handshake = self.handshake.clone();
        let control_channel = Arc::clone(&self.control_channel);
        let cancel = self.cancel.clone();
        connection.on_data_channel(Box::new(move |channel| {
            let handshake = handshake.clone();
            let control_channel = control_channel.clone();
            let cancel = cancel.clone();
            let ports = ports.clone();
            Box::pin(async move {
                let label = channel.label().to_string();
                let control = label == "CONTROL_DATA_CHANNEL";
                let file = label == "FILE_DATA_CHANNEL";
                tracing::info!(channel = %label, stream_id = channel.id(), "host business channel bound");
                if !control && !file {
                    return;
                }
                if control {
                    let weak = Arc::downgrade(&channel);
                    *control_channel.lock().unwrap() = Some(weak.clone());
                    handshake.reset();
                    let opening_handshake = handshake.clone();
                    let opening_control = control_channel.clone();
                    channel.on_open(Box::new(move || {
                        // 锁只用于取 Weak;不得跨 await 持有(回调 future 需 Send)。
                        let target = opening_control.lock().unwrap().as_ref().and_then(std::sync::Weak::upgrade);
                        Box::pin(async move {
                            // KCP 可先于 DCEP 就绪;SCTP 载体下这就是握手起点。
                            // 官方行为:被控在 CONTROL open 时主动发一条
                            // ECHO_REQUEST(echo(1, 0, true),channels.rs:167)。
                            let bytes = pb::echo_request(1, 0);
                            if let Some(channel) = target {
                                if let Err(error) = channel.send(&bytes::Bytes::from(bytes)).await {
                                    tracing::warn!(%error, "host opening ECHO_REQUEST failed");
                                }
                            }
                            opening_handshake.reset();
                        })
                    }));
                    let closing_handshake = handshake.clone();
                    let closing_control = control_channel.clone();
                    channel.on_close(Box::new(move || {
                        if let Some(channel) = closing_control.lock().unwrap().take().and_then(|weak| weak.upgrade()) {
                            tracing::info!(stream_id = channel.id(), "host CONTROL channel closed");
                        }
                        closing_handshake.reset();
                        Box::pin(async {})
                    }));
                    let message_handshake = handshake.clone();
                    let message_control = control_channel.clone();
                    let message_cancel = cancel.clone();
                    channel.on_message(Box::new(move |message| {
                        if message_cancel.is_cancelled() {
                            return Box::pin(async {});
                        }
                        let handshake = message_handshake.clone();
                        let control_channel = message_control.clone();
                        let data = message.data;
                        let reply_channel = control_channel
                            .lock()
                            .unwrap()
                            .as_ref()
                            .and_then(std::sync::Weak::upgrade);
                        Box::pin(async move {
                            match pb::receive(&data) {
                                Ok(received) => {
                                    if received.handshake_complete {
                                        handshake.mark_connected();
                                        tracing::info!("host PB handshake complete");
                                    }
                                    for response in received.messages {
                                        if let Some(channel) = &reply_channel {
                                            if let Err(error) =
                                                channel.send(&bytes::Bytes::from(response)).await
                                            {
                                                tracing::warn!(%error, "host PB response failed");
                                            }
                                        }
                                    }
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "host CONTROL message rejected");
                                }
                            }
                        })
                    }));
                }
                if file {
                    // 绑定必须在 on_data_channel 回调内立即完成:通道可能已
                    // open,on_open 不再触发(omarchyuu channels.rs:43 同款)。
                    let file_ports = ports.clone();
                    let file_cancel = cancel.clone();
                    let closing_ports = ports.clone();
                    let weak_file = Arc::downgrade(&channel);
                    let bind_channel = channel.clone();
                    Box::pin(async move {
                        if !file_cancel.is_cancelled() {
                            file_ports.bind(&bind_channel).await;
                            tracing::info!("host FILE channel bound; forwarding ready");
                        }
                    })
                    .await;
                    channel.on_close(Box::new(move || {
                        if let Some(channel) = weak_file.upgrade() {
                            closing_ports.close(&channel);
                        }
                        Box::pin(async {})
                    }));
                    let message_ports = ports.clone();
                    let message_cancel = cancel.clone();
                    let weak_message = Arc::downgrade(&channel);
                    channel.on_message(Box::new(move |message| {
                        if message_cancel.is_cancelled() {
                            return Box::pin(async {});
                        }
                        let ports = message_ports.clone();
                        let channel = weak_message.clone();
                        let data = message.data;
                        Box::pin(async move {
                            let Some(channel) = channel.upgrade() else {
                                return;
                            };
                            if let Err(error) = ports.receive(&channel, &data).await {
                                tracing::warn!(%error, "host port message rejected");
                            }
                        })
                    }));
                }
            })
        }));
        Ok(())
    }

    /// 应答主控 offer;返回带 UU SDP 属性的 answer。
    pub(crate) async fn answer(&self, sdp: String, restart: bool) -> Result<String> {
        tracing::info!(sdp_bytes = sdp.len(), restart, "host received video offer");
        // H1 固定全 SCTP:不回 a=x-uuremote-mix-kcp,KCP 载体是 H2 的开关。
        let fragments: HashSet<String> = sdp
            .lines()
            .filter_map(|line| line.strip_prefix("a=ice-ufrag:"))
            .map(str::to_owned)
            .collect();
        anyhow::ensure!(!fragments.is_empty(), "主控SDP缺少ICE代次");
        let description =
            webrtc::peer_connection::sdp::session_description::RTCSessionDescription::offer(sdp)?;
        let _ = restart;
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
                    tracing::debug!(%error, "discarded queued host ICE candidate; SDP negotiation retained");
                }
            } else if candidate
                .username_fragment
                .as_ref()
                .is_some_and(|fragment| !ice.retired.contains(fragment))
            {
                ice.pending.push_back(candidate);
            }
        }
        drop(ice);
        let sdp = self
            .core
            .answer("audio_0 video_0 video_1 video_2 video_3 video_4", None)
            .await?;
        anyhow::ensure!(!self.ended(), "画面会话已取消");
        Ok(sdp)
    }

    pub(crate) async fn add_candidate(&self, mut candidate: RTCIceCandidateInit) -> Result<()> {
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

    /// 构造 soac candidate 上行载荷;空候选返回 None。
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

    #[allow(dead_code)]
    pub(crate) fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub(crate) fn ended(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub(crate) async fn close(&self) -> Result<()> {
        self.cancel.cancel();
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

/// data-only answer 的编解码全集(screen.is_none() 分支,omarchyuu
/// peer/mod.rs:187-376):opus 111、H264 98+rtx99、H265 96+rtx97、
/// AV1 104/105 + 106/107、rs-fec 36,及官方音视频头扩展。
fn register_codecs(media: &mut MediaEngine) -> Result<()> {
    media.register_codec(
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.into(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;stereo=1;useinbandfec=1".into(),
                rtcp_feedback: vec![webrtc::rtp_transceiver::RTCPFeedback {
                    typ: "transport-cc".into(),
                    parameter: String::new(),
                }],
                ..Default::default()
            },
            payload_type: 111,
            ..Default::default()
        },
        RTPCodecType::Audio,
    )?;
    media.register_codec(
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.into(),
                clock_rate: 90_000,
                sdp_fmtp_line:
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".into(),
                rtcp_feedback: vec![
                    webrtc::rtp_transceiver::RTCPFeedback {
                        typ: "rrtr".into(),
                        parameter: String::new(),
                    },
                    webrtc::rtp_transceiver::RTCPFeedback {
                        typ: "transport-cc".into(),
                        parameter: String::new(),
                    },
                    webrtc::rtp_transceiver::RTCPFeedback {
                        typ: "nack".into(),
                        parameter: String::new(),
                    },
                    webrtc::rtp_transceiver::RTCPFeedback {
                        typ: "nack".into(),
                        parameter: "pli".into(),
                    },
                    webrtc::rtp_transceiver::RTCPFeedback {
                        typ: "ccm".into(),
                        parameter: "fir".into(),
                    },
                ],
                ..Default::default()
            },
            payload_type: 98,
            ..Default::default()
        },
        RTPCodecType::Video,
    )?;
    let codecs: [(&str, u8, &str, &str); 3] = [
        (MIME_TYPE_HEVC, 96, "", "apt=96"),
        ("video/AV1", 104, "profile=0;level-idx=17;tier=0", "apt=104"),
        ("video/AV1", 106, "profile=1;level-idx=17;tier=0", "apt=106"),
    ];
    for (mime, pt, fmtp, rtx) in codecs {
        media.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: mime.into(),
                    clock_rate: 90_000,
                    sdp_fmtp_line: fmtp.into(),
                    rtcp_feedback: vec![
                        webrtc::rtp_transceiver::RTCPFeedback {
                            typ: "rrtr".into(),
                            parameter: String::new(),
                        },
                        webrtc::rtp_transceiver::RTCPFeedback {
                            typ: "transport-cc".into(),
                            parameter: String::new(),
                        },
                        webrtc::rtp_transceiver::RTCPFeedback {
                            typ: "nack".into(),
                            parameter: String::new(),
                        },
                        webrtc::rtp_transceiver::RTCPFeedback {
                            typ: "nack".into(),
                            parameter: "pli".into(),
                        },
                        webrtc::rtp_transceiver::RTCPFeedback {
                            typ: "ccm".into(),
                            parameter: "fir".into(),
                        },
                    ],
                    ..Default::default()
                },
                payload_type: pt,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;
        media.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/rtx".into(),
                    clock_rate: 90_000,
                    sdp_fmtp_line: rtx.into(),
                    ..Default::default()
                },
                payload_type: pt + 1,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;
    }
    media.register_codec(
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "video/rtx".into(),
                clock_rate: 90_000,
                sdp_fmtp_line: "apt=98".into(),
                ..Default::default()
            },
            payload_type: 99,
            ..Default::default()
        },
        RTPCodecType::Video,
    )?;
    for uri in [
        "urn:ietf:params:rtp-hdrext:ssrc-audio-level",
        "http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time",
        "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01",
        "urn:ietf:params:rtp-hdrext:sdes:mid",
    ] {
        media.register_header_extension(
            RTCRtpHeaderExtensionCapability { uri: uri.into() },
            RTPCodecType::Audio,
            None,
        )?;
    }
    for uri in [
        "http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time",
        "urn:ietf:params:rtp-hdrext:toffset",
        "http://www.webrtc.org/experiments/rtp-hdrext/video-timing",
        "http://www.webrtc.org/experiments/rtp-hdrext/video-frame-sending-delay",
        "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01",
        "urn:ietf:params:rtp-hdrext:sdes:mid",
        "http://www.webrtc.org/experiments/rtp-hdrext/color-space",
        "http://www.webrtc.org/experiments/rtp-hdrext/video-capture-index",
        "http://www.webrtc.org/experiments/rtp-hdrext/video-frame-is-new-frame",
        "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay",
    ] {
        media.register_header_extension(
            RTCRtpHeaderExtensionCapability { uri: uri.into() },
            RTPCodecType::Video,
            None,
        )?;
    }
    media.register_codec(
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "video/rs-fec-cm256".into(),
                clock_rate: 90_000,
                sdp_fmtp_line: "max-k=109;repair-window=10000000;rtx-as-source=1".into(),
                ..Default::default()
            },
            payload_type: 36,
            ..Default::default()
        },
        RTPCodecType::Video,
    )?;
    Ok(())
}
