//! 被控侧 PB CONTROL 握手状态(omarchyuu `ReportRoutes.handshake_*` +
//! `stream_control::handshake` 的被控等价物)。
//!
//! 主控 `wait_port_mapping_ready` 等的是「它收到我们的 ECHO_RESPONSE」;
//! 这里维护的是对称方向:我们收到主控 ECHO_RESPONSE 即视为握手完成。

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Default)]
pub(crate) struct HostHandshake {
    connected: Arc<AtomicBool>,
}

impl HostHandshake {
    pub(crate) fn mark_connected(&self) {
        self.connected.store(true, Ordering::Release);
    }

    /// 握手尚未完成或通道更换后会复位。
    pub(crate) fn reset(&self) {
        self.connected.store(false, Ordering::Release);
    }

    pub(crate) fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }
}
