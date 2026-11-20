//! 被控在线生命周期(omarchyuu `session/presence.rs` 的 host 路径裁剪版):
//! 创建房间 → 信令 Host 角色 → keep-alive;失联按官方退避策略重建。
//!
//! 无 HTTP 心跳:Engine.IO ping/pong + 30s reconnect-key 刷新都在信令层。
//! omarchyuu 的多进程 presence 所有权锁(reserve_presence)与 assist/WOL
//! 常驻推送不在此版本;多实例互踢问题留给后续阶段。

use super::HostState;
use super::client::HostClient;
use crate::backend::nrd::{self, CallError};
use crate::signal::{self, SignalFailure, SignalPushHandler, SignalRole, SocketState};
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

/// 从推送事件解析「本机被解绑」;其他推送 v1 一律忽略。
fn parse_unbind(push: &Value, device_id: &str) -> bool {
    push.get("type").and_then(Value::as_str) == Some("device_unbind")
        && push
            .get("data")
            .and_then(|data| data.get("device_id"))
            .and_then(Value::as_str)
            .is_some_and(|id| id == device_id)
}

pub(super) async fn run_presence(
    client: HostClient,
    state: watch::Sender<HostState>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut policy = HostRoomRetry::default();
    let mut initial_requests = 0;
    let mut backoff = false;
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let _ = state.send(if backoff {
            HostState::Reconnecting
        } else {
            HostState::Connecting
        });
        // 39C5B0:从未被控制过的设备发 -1。
        let room = match client.create_host_room(-1).await {
            Ok(room) => {
                policy.room_response(&room);
                room
            }
            Err(error) => {
                if let CallError::Business(business) = &error
                    && HostClient::invalid_credentials(business.code)
                {
                    // 凭据已被服务端拒绝(解绑/登出/过期):清会话回到登录页。
                    client.retire();
                    let _ = state.send(HostState::Off);
                    return Err(anyhow::anyhow!("{error}"));
                }
                let delay = if backoff {
                    policy.failed_backoff_request()
                } else {
                    initial_requests += 1;
                    if initial_requests >= 3 {
                        return Err(anyhow::anyhow!("{error}"));
                    }
                    Duration::from_millis(500)
                };
                tracing::warn!("在线房间请求失败:{error:#};{} ms 后重试", delay.as_millis());
                tokio::select! {
                    _ = cancel.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(delay) => {}
                }
                continue;
            }
        };
        let room_cancel = cancel.child_token();
        let push_client = client.clone();
        let push_cancel = room_cancel.clone();
        let device_id = client.device_id().to_string();
        let observer: SignalPushHandler = Arc::new(move |push| {
            if push_cancel.is_cancelled() || !push_client.is_active() {
                return;
            }
            if parse_unbind(push, &device_id) {
                // Server 398A50 -> 3B5540:精确匹配当前设备才退代。
                tracing::info!("current virtual device was unbound; retiring account generation");
                push_client.retire();
            }
        });
        let session = signal::SignalSession::connect_observed(
            room,
            SignalRole::Host,
            &room_cancel,
            Some(observer),
        )
        .await;
        let result = match session {
            Ok(session) => {
                if let Err(error) = client.set_controllable(true).await {
                    tracing::warn!(%error, "上报可控位失败;房间仍然保持");
                }
                let _ = state.send(HostState::Online);
                let mut socket_state = session.socket_state();
                let mut state_open = true;
                let (shutdown, shutdown_receiver) = oneshot::channel();
                let alive = session.keep_alive_host(
                    shutdown_receiver,
                    signal::host::Session::new(client.clone()),
                );
                tokio::pin!(alive);
                loop {
                    tokio::select! {
                        result = &mut alive => break result,
                        _ = cancel.cancelled() => { let _ = shutdown.send(()); break alive.await; }
                        state_change = socket_state.changed(), if state_open => {
                            if state_change.is_err() { state_open = false; continue; }
                            let socket = *socket_state.borrow_and_update();
                            let presence = match socket {
                                SocketState::Connected => HostState::Online,
                                SocketState::Connecting | SocketState::Reconnecting => HostState::Reconnecting,
                                SocketState::Closed => HostState::Offline,
                            };
                            let _ = state.send(presence);
                        }
                    }
                }
            }
            Err(error) => Err(error),
        };
        room_cancel.cancel();
        let _ = client.set_controllable(false).await;
        if cancel.is_cancelled() {
            let _ = state.send(HostState::Off);
            return Ok(());
        }
        if matches!(
            result
                .as_ref()
                .err()
                .and_then(|error| error.downcast_ref::<SignalFailure>()),
            Some(SignalFailure::Kicked)
        ) {
            return result;
        }
        let delay = policy.lost_room();
        backoff = true;
        initial_requests = 0;
        if let Err(error) = result {
            tracing::warn!("在线房间失联:{error:#};{} 秒后重建", delay.as_secs());
        }
        tokio::select! {
            _ = cancel.cancelled() => { let _ = state.send(HostState::Off); return Ok(()); }
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

// GameViewerServer 2D5DE0, 3CA500..3CA680 and 3BB060/3BB490.
// HTTP-response and socket-open time are distinct: >=30s starts a random
// 1..5s backoff; repeated short failures double delay up to the server limit.
struct HostRoomRetry {
    delay_seconds: u32,
    maximum_seconds: u32,
    last_room_response: Option<Instant>,
}
impl Default for HostRoomRetry {
    fn default() -> Self {
        Self {
            delay_seconds: 0,
            maximum_seconds: 300,
            last_room_response: None,
        }
    }
}
impl HostRoomRetry {
    fn room_response(&mut self, room: &nrd::RoomSession) {
        if room.max_reconnect_delta > 0 {
            self.maximum_seconds = room.max_reconnect_delta as u32;
        }
        self.last_room_response = Some(Instant::now());
    }
    fn lost_room(&mut self) -> Duration {
        if self
            .last_room_response
            .is_none_or(|time| time.elapsed() >= Duration::from_secs(30))
        {
            self.delay_seconds = rand::random_range(1..=5);
        } else {
            self.delay_seconds = self
                .delay_seconds
                .saturating_mul(2)
                .min(self.maximum_seconds);
        }
        self.delay_seconds = self.delay_seconds.max(1);
        Duration::from_secs(self.delay_seconds as u64)
    }
    fn failed_backoff_request(&mut self) -> Duration {
        self.delay_seconds = self
            .delay_seconds
            .saturating_mul(2)
            .min(self.maximum_seconds)
            .max(1);
        Duration::from_secs(self.delay_seconds as u64)
    }
}
