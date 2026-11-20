//! TCP 流与 UDP 数据报经认证的 FILE 通道转发(omarchyuu
//! `features/port_mapping` 的被控侧子集)。
//!
//! `Transport` 承载分帧/背压/路由;主控侧的本地监听(run_rule/probe/
//! service)属 Phase C,不在本模块。

pub(crate) mod host;
pub(crate) mod policy;

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use prost::Message;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex, Weak,
        atomic::Ordering,
    },
    time::Duration,
};
use tokio::{
    sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc},
};
use tokio_util::sync::CancellationToken;
use webrtc::data_channel::{RTCDataChannel, data_channel_state::RTCDataChannelState};

pub(crate) const CHUNK: usize = 524_160;
pub(crate) const BUFFER: usize = 8 * 1024 * 1024;
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const WIRE_LIMIT: usize = 524_288;

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct Frame {
    #[prost(uint64, tag = "1")]
    pub(crate) session_id: u64,
    #[prost(uint64, tag = "2")]
    pub(crate) rule_id: u64,
    #[prost(uint64, tag = "3")]
    pub(crate) stream_id: u64,
    #[prost(int32, tag = "4")]
    pub(crate) kind: i32,
    #[prost(bytes = "bytes", tag = "5")]
    pub(crate) payload: Bytes,
}

pub(crate) struct Incoming {
    pub(crate) frame: Frame,
    pub(crate) _permit: OwnedSemaphorePermit,
    pub(crate) _global: OwnedSemaphorePermit,
}
pub(crate) struct Route {
    pub(crate) sender: mpsc::Sender<Incoming>,
    pub(crate) budget: Arc<Semaphore>,
    pub(crate) stop: CancellationToken,
    pub(crate) remote_fin: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) fault: Arc<Mutex<Option<String>>>,
}

/// 值 16..=20 是 OpenUUYC 数据报扩展;官方 peer 会忽略而不是为 UDP 请求开 TCP。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Protocol {
    Tcp,
    Udp,
}
impl Protocol {
    pub(crate) fn syn(self) -> i32 {
        if self == Self::Tcp {
            0
        } else {
            16
        }
    }
    pub(crate) fn ack(self) -> i32 {
        if self == Self::Tcp {
            3
        } else {
            17
        }
    }
    pub(crate) fn fin(self) -> i32 {
        if self == Self::Tcp {
            1
        } else {
            19
        }
    }
    pub(crate) fn from_kind(kind: i32) -> Self {
        if kind == 16 {
            Self::Udp
        } else {
            Self::Tcp
        }
    }
}

pub(crate) struct Transport {
    channel: Mutex<Weak<RTCDataChannel>>,
    routes: Mutex<HashMap<(u64, u64), Route>>,
    changed: Notify,
    send_lock: tokio::sync::Mutex<()>,
    budget: Arc<Semaphore>,
}
impl Default for Transport {
    fn default() -> Self {
        Self {
            channel: Mutex::new(Weak::new()),
            routes: Mutex::default(),
            changed: Notify::new(),
            send_lock: tokio::sync::Mutex::new(()),
            budget: Arc::new(Semaphore::new(32 * 1024 * 1024)),
        }
    }
}

impl Transport {
    pub(crate) fn register(&self, key: (u64, u64), route: Route) -> Result<()> {
        let mut routes = lock(&self.routes);
        ensure!(routes.len() < 128, "端口转发总连接数已达上限");
        ensure!(!routes.contains_key(&key), "端口转发编号重复");
        routes.insert(key, route);
        Ok(())
    }
    pub(crate) fn bind(&self, channel: &Arc<RTCDataChannel>) {
        *lock(&self.channel) = Arc::downgrade(channel);
        self.changed.notify_waiters();
    }
    pub(crate) fn wake(&self) {
        self.changed.notify_waiters();
    }
    pub(crate) fn close(&self) {
        *lock(&self.channel) = Weak::new();
        for (_, route) in lock(&self.routes).drain() {
            route.stop.cancel();
        }
        self.changed.notify_waiters();
    }
    pub(crate) fn ready(&self) -> bool {
        lock(&self.channel)
            .upgrade()
            .is_some_and(|c| c.ready_state() == RTCDataChannelState::Open)
    }
    #[allow(dead_code)]
    pub(crate) async fn wait_ready(&self) -> Result<()> {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let wake = self.changed.notified();
                tokio::pin!(wake);
                wake.as_mut().enable();
                if self.ready() {
                    return;
                }
                wake.await;
            }
        })
        .await
        .context("端口转发数据通道未就绪")
    }
    pub(crate) fn receive(&self, bytes: &[u8]) -> Result<()> {
        ensure!(bytes.len() < WIRE_LIMIT, "端口转发消息过大");
        let Some(frame) = crate::pb::decode_port_mapping(bytes)? else {
            return Ok(());
        };
        let frame = Frame::decode(frame.as_slice()).context("无效端口转发消息")?;
        self.deliver(frame)
    }
    pub(crate) fn deliver(&self, frame: Frame) -> Result<()> {
        // 未知 kind 与未知流不得产生本地副作用。
        if !matches!(frame.kind, 1..=4 | 17..=20) {
            return Ok(());
        }
        let routes = lock(&self.routes);
        let Some(route) = routes.get(&(frame.rule_id, frame.stream_id)) else {
            return Ok(());
        };
        if matches!(frame.kind, 1 | 19) {
            route.remote_fin.store(true, Ordering::Release);
        }
        let size = frame.payload.len().max(1);
        let Ok(permit) = Arc::clone(&route.budget).try_acquire_many_owned(size as u32) else {
            *lock(&route.fault) = Some("端口转发单连接接收缓存已满".into());
            route.stop.cancel();
            return Ok(());
        };
        let Ok(global) = self.budget.clone().try_acquire_many_owned(size as u32) else {
            *lock(&route.fault) = Some("端口转发总接收缓存已满".into());
            route.stop.cancel();
            return Ok(());
        };
        if route
            .sender
            .try_send(Incoming {
                frame,
                _permit: permit,
                _global: global,
            })
            .is_err()
        {
            *lock(&route.fault) = Some("端口转发接收队列已满".into());
            route.stop.cancel();
        }
        Ok(())
    }
    pub(crate) async fn send(
        &self,
        rule: u64,
        stream: u64,
        kind: i32,
        payload: Bytes,
    ) -> Result<()> {
        let frame = Frame {
            session_id: 1,
            rule_id: rule,
            stream_id: stream,
            kind,
            payload,
        };
        let bytes = Bytes::from(crate::pb::encode_port_mapping(frame.encode_to_vec()));
        ensure!(bytes.len() <= WIRE_LIMIT, "端口转发消息过大");
        let channel = lock(&self.channel)
            .upgrade()
            .context("端口转发连接已关闭")?;
        // 预算检查与提交串行化:满了就停读 TCP,SCTP 自身也有界。
        tokio::time::timeout(TIMEOUT, async {
            let _guard = self.send_lock.lock().await;
            loop {
                let wake = self.changed.notified();
                tokio::pin!(wake);
                wake.as_mut().enable();
                ensure!(
                    channel.ready_state() == RTCDataChannelState::Open,
                    "端口转发连接已关闭"
                );
                if channel.buffered_amount().await + bytes.len() <= BUFFER {
                    break;
                }
                wake.await;
            }
            channel.send(&bytes).await?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("端口转发发送超时")??;
        Ok(())
    }
}
