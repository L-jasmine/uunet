//! 被控服务(最小被控端):在线登记 + 只接受端口转发。
//!
//! 模块边界:`client` 是账号侧门面,`presence` 管在线生命周期,
//! `settings` 是本机允许策略,`access`(H2)是会话内权限。

pub mod access;
pub mod client;
pub mod presence;
pub mod settings;

use anyhow::Result;
use client::HostClient;
use std::sync::OnceLock;
use tokio_util::sync::CancellationToken;

/// 被控在线状态,喂给 TUI。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostState {
    Off,
    Connecting,
    Online,
    Reconnecting,
    Offline,
}

/// 运行中的被控服务句柄:状态 watch + 停止开关。
#[derive(Clone)]
pub struct HostHandle {
    pub state: tokio::sync::watch::Receiver<HostState>,
    #[allow(dead_code)]
    cancel: CancellationToken,
}

impl HostHandle {
    /// 进程内单例常驻,目前无停止入口;保留开关供后续使用。
    #[allow(dead_code)]
    pub async fn stop(self) {
        self.cancel.cancel();
    }
}

static HANDLE: OnceLock<HostHandle> = OnceLock::new();

/// 启动被控服务(进程内单例);重复调用返回已有句柄的克隆。
pub async fn start_service() -> Result<HostHandle> {
    if let Some(handle) = HANDLE.get() {
        return Ok(handle.clone());
    }
    let client = HostClient::new().await?;
    let cancel = CancellationToken::new();
    let (state_tx, state_rx) = tokio::sync::watch::channel(HostState::Connecting);
    let task_cancel = cancel.clone();
    let state_tx_on_error = state_tx.clone();
    tokio::spawn(async move {
        let outcome = tokio::select! {
            _ = task_cancel.cancelled() => Ok(()),
            result = presence::run_presence(client, state_tx, task_cancel.clone()) => result,
        };
        // run_presence 的失败此前被静默丢弃,在线问题无从排查。
        if let Err(error) = outcome {
            tracing::error!("被控在线任务退出:{error:#}");
            state_tx_on_error.send_replace(HostState::Offline);
        }
    });
    let handle = HostHandle {
        state: state_rx,
        cancel,
    };
    let _ = HANDLE.set(handle.clone());
    Ok(handle)
}

/// 已运行服务的句柄(未启动返回 None)。
pub fn running_handle() -> Option<HostHandle> {
    HANDLE.get().cloned()
}

/// `--host` 启动模式:等登录态就绪后自动拉起被控服务。
/// mock 模式不启动(离线没有房间服务)。
pub fn enable_at_startup() {
    tokio::spawn(async move {
        let mut logged = crate::backend::logged_in();
        if *logged.borrow_and_update() {
            start_and_log().await;
            return;
        }
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return,
                changed = logged.changed() => {
                    if changed.is_err() { return; }
                    if *logged.borrow_and_update() {
                        start_and_log().await;
                        return;
                    }
                }
            }
        }
    });
}

async fn start_and_log() {
    if crate::backend::mock_mode() {
        tracing::info!("mock 模式不启动被控服务");
        return;
    }
    match start_service().await {
        Ok(_) => tracing::info!("被控服务已启动(在线登记 + 仅端口转发)"),
        Err(error) => tracing::error!("被控服务启动失败:{error:#}"),
    }
}
