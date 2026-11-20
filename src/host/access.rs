//! 被控会话内的端口转发许可(uunet 简化版)。
//!
//! uunet 被控端即纯转发节点:接受会话即允许转发,不存在 omarchyuu 的
//! 「允许被控 / 允许端口转发」分级开关;拨号目标由白名单实时把关
//! (`forward::policy`)。Lease 保留接口形态以对齐 forward::host 的调用。

use std::sync::{Arc, Mutex, OnceLock};

static ACTIVE: OnceLock<Lease> = OnceLock::new();

#[derive(Clone)]
pub(crate) struct Lease {
    status: Arc<Mutex<crate::forward::host::Status>>,
}

impl Default for Lease {
    fn default() -> Self {
        Self::new()
    }
}

impl Lease {
    pub(crate) fn new() -> Self {
        static ACTIVE: OnceLock<Lease> = OnceLock::new();
        let lease = Self {
            status: Arc::new(Mutex::new(crate::forward::host::Status::default())),
        };
        let _ = ACTIVE.set(lease.clone());
        lease
    }

    /// 转发 actor 是否运行:被控会话存在即为真(会话结束由 stop 取消)。
    pub(crate) fn requested(&self) -> bool {
        true
    }

    /// (允许端口转发, 修订号)——uunet 恒为允许;白名单按连接实时检查。
    pub(crate) fn port_policy(&self) -> (bool, u64) {
        (true, 0)
    }

    pub(crate) fn port_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        static NEVER: std::sync::OnceLock<tokio::sync::watch::Sender<u64>> = std::sync::OnceLock::new();
        NEVER
            .get_or_init(|| tokio::sync::watch::channel(0_u64).0)
            .subscribe()
    }

    pub(crate) fn port_status(&self, status: crate::forward::host::Status) {
        *self.status.lock().unwrap() = status;
    }

    /// 供 TUI 读取当前连接统计。
    pub(crate) fn status(&self) -> crate::forward::host::Status {
        self.status.lock().unwrap().clone()
    }
}

/// 最近一次被控会话的统计(无会话时为零值)。
pub(crate) fn current_status() -> crate::forward::host::Status {
    ACTIVE
        .get()
        .map(|lease| lease.status())
        .unwrap_or_default()
}
