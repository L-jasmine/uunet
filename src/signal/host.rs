//! Host role of the authenticated online room(uunet 裁剪版)。
//!
//! 拷自 omarchyuu `transport/signal/host.rs`,只保留端口转发(kind 9)
//! 所需路径:be-controlled 解析 → 被控 Peer → soac 应答 → left/released
//! 清理。电源/协助/WOL/终端/文件/采集/编码全部不在被控面;capability
//! 上报空 display_info 与空编解码列表(data-only 模板,signal/host.rs:483)。

use super::*;
use anyhow::Context as _;
use crate::capability::DeviceCapability;
use crate::host::client::HostClient;
use crate::host::settings;
use crate::peer::Peer;
use crate::pb::ConnectOptions;
use prost::Message as _;
use serde_json::Value;
use std::collections::VecDeque;

pub(crate) enum Event {
    Candidate(webrtc::ice_transport::ice_candidate::RTCIceCandidateInit),
    Poll,
}

pub(crate) struct Session {
    client: HostClient,
    settings: settings::Shared,
    peer: Option<Peer>,
    candidates: tokio::sync::mpsc::UnboundedReceiver<
        webrtc::ice_transport::ice_candidate::RTCIceCandidateInit,
    >,
    client_id: String,
    ice_id: String,
    app_control_id: String,
    initialized: bool,
}

impl Session {
    pub(crate) fn new(client: HostClient) -> Self {
        Self {
            client,
            settings: settings::shared(),
            peer: None,
            candidates: tokio::sync::mpsc::unbounded_channel().1,
            client_id: String::new(),
            ice_id: String::new(),
            app_control_id: String::new(),
            initialized: false,
        }
    }

    /// 本机策略:总开关 + 端口转发都允许,才接受映射会话。
    fn allowed(&self) -> bool {
        let settings = self.settings.read().unwrap();
        settings.allow && settings.port_mapping
    }

    pub(crate) async fn next(&mut self) -> Event {
        if !self.initialized {
            self.initialized = true;
            // 首次 poll 触发一次策略检查(peer 不存在时是无害空转)。
            return Event::Poll;
        }
        tokio::select! {
            candidate = async { self.candidates.recv().await } => {
                candidate.map_or(Event::Poll, Event::Candidate)
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => Event::Poll,
        }
    }

    pub(crate) async fn apply(&mut self, event: Event, signal: &mut SignalSession) -> Result<()> {
        match event {
            Event::Candidate(candidate) => {
                let Some(peer) = self.peer.as_ref() else {
                    return Ok(());
                };
                let Some(candidate) = peer.local_candidate(candidate).await else {
                    return Ok(());
                };
                let payload = serde_json::json!({"client_id": self.client_id, "data": {
                    "app_control_id": self.app_control_id, "ice_id": self.ice_id,
                    "ice_network_type": 0, "type": "candidate", "candidate": candidate}});
                signal
                    .send_text(encode_event("soac", &[payload], None)?)
                    .await?;
            }
            Event::Poll => {
                // 策略关闭或对端已死:清理本连接;房间与在线不受影响
                // (omarchyuu signal/host.rs:137 的同款边界)。
                if self.peer.is_some() && (!self.allowed() || self.peer.as_ref().is_some_and(Peer::ended)) {
                    self.end_control(signal).await;
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn event(
        &mut self,
        event: &str,
        args: &[Value],
        binary: &[Vec<u8>],
        signal: &mut SignalSession,
    ) -> Result<()> {
        if !self.client.is_active() {
            return Ok(());
        }
        if !self.allowed() {
            if event == "be-controlled" {
                // 云端设备列表可能滞后:明确拒绝,避免主控空等。
                self.end_control(signal).await;
                Self::notify_termination(signal).await;
                tracing::info!("host incoming connection refused by local access policy");
            }
            return Ok(());
        }
        match event {
            "be-controlled" => self.be_controlled(args, binary, signal).await,
            "soac" => {
                let value = args.first().context("缺少SOAC")?;
                if value
                    .get("client_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id != self.client_id)
                {
                    return Ok(());
                }
                let data = value.get("data").context("缺少SOAC data")?;
                if data.get("ice_id").and_then(Value::as_str) != Some(self.ice_id.as_str())
                    || data
                        .get("app_control_id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id != self.app_control_id)
                {
                    return Ok(());
                }
                self.soac(data, signal).await
            }
            "left" | "released" => {
                // AA81A0:非空 ice_id 才释放;三项都匹配才关 Peer。
                if args
                    .first()
                    .and_then(|v| v.get("ice_id"))
                    .and_then(Value::as_str)
                    != Some(self.ice_id.as_str())
                    || self.ice_id.is_empty()
                {
                    return Ok(());
                }
                if args.first().is_some_and(|v| {
                    v.get("ice_id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id != self.ice_id)
                        || v.get("app_control_id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| id != self.app_control_id)
                }) {
                    return Ok(());
                }
                if args
                    .first()
                    .and_then(|v| v.get("client_id"))
                    .and_then(Value::as_str)
                    .is_some_and(|id| id != self.client_id)
                {
                    return Ok(());
                }
                if let Some(peer) = self.peer.take() {
                    tracing::info!(event, "host matching signaling release received");
                    peer.close().await?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn be_controlled(
        &mut self,
        args: &[Value],
        binary: &[Vec<u8>],
        signal: &mut SignalSession,
    ) -> Result<()> {
        tracing::info!("host received authenticated be-controlled event");
        let value = args.first().context("缺少被控会话参数")?;
        let required = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty() && v.len() <= 256)
                .map(str::to_owned)
                .context("被控会话标识缺失或无效")
        };
        let client_id = required("client_id")?;
        let ice_id = required("ice_id")?;
        let app_control_id = required("app_control_id")?;
        let index = value
            .get("app_data")
            .filter(|v| v.get("_placeholder").and_then(Value::as_bool) == Some(true))
            .and_then(|v| v.get("num"))
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .context("缺少ConnectOptions二进制引用")?;
        let options = ConnectOptions::decode(
            binary
                .get(index)
                .context("缺少ConnectOptions二进制内容")?
                .as_slice(),
        )?;
        // uunet 只服务端口转发(kind 9,普通连接类型 1)。
        let ports_only = options.kind == crate::pb::CONNECT_KIND_PORTS;
        tracing::info!(
            client_type = options.client_type,
            kind = options.kind,
            connect_type = options.connect_type,
            controller_device = %options.device_id,
            "host incoming connection request"
        );
        if !ports_only || options.connect_type != 1 {
            Self::notify_termination(signal).await;
            anyhow::bail!("uunet 被控端只接受端口转发会话");
        }
        anyhow::ensure!(
            self.allowed() && self.settings.read().unwrap().port_mapping,
            "本机未允许端口转发"
        );
        let servers: Vec<ControlIceServer> = serde_json::from_value(
            value
                .get("iceServers")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        )?;
        let servers = servers
            .into_iter()
            .map(|s| webrtc::ice_transport::ice_server::RTCIceServer {
                urls: vec![s.urls],
                username: s.username,
                credential: s.credential,
                ..Default::default()
            })
            .collect();
        let relay = value
            .get("force_relay")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // 上一个会话还在才清理(end_control 空会话早退,不误发 clear_out)。
        if self.peer.is_some() {
            self.end_control(signal).await;
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let peer_started = std::time::Instant::now();
        let (peer, candidates) = Peer::new(servers, relay, cancel).await?;
        tracing::info!(peer_ms = peer_started.elapsed().as_millis(), "host connection prepared");
        self.client_id = client_id;
        self.ice_id = ice_id;
        self.app_control_id = app_control_id;
        self.candidates = candidates;
        self.peer = Some(peer);
        self.send_capability(signal).await?;
        Ok(())
    }

    async fn soac(&mut self, data: &Value, signal: &mut SignalSession) -> Result<()> {
        let peer = self.peer.as_ref().context("没有活动被控会话")?;
        match data.get("type").and_then(Value::as_str) {
            Some("offer" | "restart_ice") => {
                let sdp = data
                    .get("sdp")
                    .and_then(Value::as_str)
                    .context("缺少offer SDP")?;
                let restart = data.get("type").and_then(Value::as_str) == Some("restart_ice");
                let answer = peer.answer(sdp.to_owned(), restart).await?;
                let attachment = gzip_sdp(&answer)?;
                let payload = serde_json::json!({"client_id": self.client_id, "data": {
                    "app_control_id": self.app_control_id, "ice_id": self.ice_id,
                    "type": if restart { "restart_ice" } else { "answer" },
                    "ice_network_type": 0, "sdp": "",
                    "gzip_sdp": {"_placeholder": true, "num": 0}}});
                signal.send_binary_packet(
                    encode_binary_event("soac", &[payload], None, 1)?,
                    attachment,
                )?;
            }
            Some("candidate") => {
                peer.add_candidate(serde_json::from_value(
                    data.get("candidate").cloned().context("缺少candidate")?,
                )?)
                .await?
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) async fn signaling_restored(&mut self, signal: &mut SignalSession) -> Result<()> {
        // 信令 socket 丢失会丢掉排队消息;capability 幂等,重发无害。
        if self.peer.is_some() {
            self.send_capability(signal).await?;
        }
        Ok(())
    }

    /// 有活动会话才通知 clear_out(omarchyuu end_control 的早退语义:
    /// 空会话发 clear_out 会让主控误以为被控拒接,官方客户端会立刻放弃)。
    async fn end_control(&mut self, signal: &mut SignalSession) {
        let Some(peer) = self.peer.take() else {
            return;
        };
        Self::notify_termination(signal).await;
        if let Err(error) = peer.close().await {
            tracing::warn!(%error, "host peer close failed");
        }
    }

    /// clear_out:只清本房间观看者的状态;本地销毁 Peer 前发送
    /// (S 2F97E0 -> 313FC0 -> ControlledInterface[13])。
    async fn notify_termination(signal: &mut SignalSession) {
        let submitted = async {
            let packet = encode_event("clear_out", &[], None)?;
            signal
                .transport
                .send_connected(vec![tokio_tungstenite::tungstenite::Message::Text(
                    packet.into(),
                )])
                .await
        }
        .await;
        match submitted {
            Ok(()) => tracing::info!("host active control termination submitted"),
            Err(error) => {
                tracing::warn!(%error, "host termination notice unavailable; stopping locally without replay")
            }
        }
    }

    /// data-only capability:空 display_info、空编解码列表
    /// (omarchyuu signal/host.rs:754,主控对数据型会话不消费视频)。
    async fn send_capability(&mut self, signal: &mut SignalSession) -> Result<()> {
        let capability = DeviceCapability {
            preferred_codec: None,
            openuuyc_sdr_10bit: true,
            ice_id: self.ice_id.clone(),
            display_info: Vec::new(),
            video_codec_capability: Vec::new(),
        };
        let payload = serde_json::json!({"client_id": self.client_id, "data": {
            "type": "device_capability", "device_capability": capability}});
        signal
            .send_text(encode_event("forward_setting", &[payload], None)?)
            .await?;
        tracing::info!(ice_id = %self.ice_id, "host device_capability announced (data-only)");
        Ok(())
    }

    pub(crate) async fn close(mut self) {
        if let Some(peer) = self.peer.take() {
            tracing::info!("host signaling owner closing active session");
            if let Err(error) = peer.close().await {
                tracing::warn!(%error, "host peer close failed");
            }
        }
    }
}
