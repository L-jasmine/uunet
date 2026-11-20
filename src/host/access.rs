//! 被控会话内的端口转发许可(omarchyuu `features/host/access.rs` 的
//! Lease 端口部分极简版)。
//!
//! - `requested()`:总开关(allow)——关掉即终止整个转发 actor;
//! - `port_policy()`:(允许端口转发, 修订号)——修订号变化会使所有
//!   在途连接失效,保证开关变更即时生效;
//! - 设置变更由 400ms 轮询捕获(host.json 也可被手工编辑)。

use super::settings;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::watch;

#[derive(Clone)]
pub(crate) struct Lease {
    settings: settings::Shared,
    revision: Arc<AtomicU64>,
    revision_tx: watch::Sender<u64>,
    status: Arc<Mutex<crate::forward::host::Status>>,
}

static ACTIVE: OnceLock<Lease> = OnceLock::new();

/// 最近一次被控会话的 Lease(无会话时返回零值统计)。
pub(crate) fn current_status() -> crate::forward::host::Status {
    ACTIVE.get().map(Lease::status).unwrap_or_default()
}

impl Lease {
    pub(crate) fn new(settings: settings::Shared) -> Self {
        let (revision_tx, _) = watch::channel(0_u64);
        let lease = Self {
            settings,
            revision: Arc::new(AtomicU64::new(0)),
            revision_tx,
            status: Arc::new(Mutex::new(crate::forward::host::Status::default())),
        };
        lease.spawn_settings_watcher();
        let _ = ACTIVE.set(lease.clone());
        lease
    }

    fn spawn_settings_watcher(&self) {
        let settings = self.settings.clone();
        let revision = self.revision.clone();
        let revision_tx = self.revision_tx.clone();
        tokio::spawn(async move {
            let mut last = {
                let settings = settings.read().unwrap();
                (settings.allow, settings.port_mapping)
            };
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                let now = {
                    let settings = settings.read().unwrap();
                    (settings.allow, settings.port_mapping)
                };
                if now != last {
                    last = now;
                    let value = revision.fetch_add(1, Ordering::AcqRel) + 1;
                    revision_tx.send_replace(value);
                }
            }
        });
    }

    /// 总开关:关闭时转发 actor 整体退出(omarchyuu 同名语义)。
    pub(crate) fn requested(&self) -> bool {
        self.settings.read().unwrap().allow
    }

    /// (是否允许端口转发, 修订号)。
    pub(crate) fn port_policy(&self) -> (bool, u64) {
        let settings = self.settings.read().unwrap();
        (
            settings.allow && settings.port_mapping,
            self.revision.load(Ordering::Acquire),
        )
    }

    pub(crate) fn port_changes(&self) -> watch::Receiver<u64> {
        self.revision_tx.subscribe()
    }

    pub(crate) fn port_status(&self, status: crate::forward::host::Status) {
        *self.status.lock().unwrap() = status;
    }

    /// 供 TUI(H3)读取当前连接统计。
    #[allow(dead_code)]
    pub(crate) fn status(&self) -> crate::forward::host::Status {
        self.status.lock().unwrap().clone()
    }
}
