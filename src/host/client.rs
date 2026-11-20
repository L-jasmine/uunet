//! 被控身份的账号侧门面(omarchyuu `session/host_client.rs` 的极简版)。
//!
//! 只保留被控在线需要的四个动作:设备号、创建房间、可控位、账号失效清理。
//! 信令传输不在这一层(在 `crate::signal`)。

use crate::backend::{
    identity,
    nrd::{self},
    session,
};
use anyhow::{Result, anyhow};

#[derive(Clone)]
pub struct HostClient {
    device_id: String,
}

impl HostClient {
    /// 从本机身份文件取设备号;要求已完成设备注册。
    pub async fn new() -> Result<Self> {
        let identity = identity::ensure_registered().await?;
        if identity.device_id.is_empty() {
            return Err(anyhow!("本机设备尚未注册,无法担任被控端"));
        }
        Ok(Self {
            device_id: identity.device_id,
        })
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// 带账号态的 API 客户端;会话文件丢失/失效时报错。
    async fn api(&self) -> Result<nrd::NrdClient> {
        let stored = session::load()?.ok_or_else(|| anyhow!("未登录:没有已保存的会话"))?;
        let identity = identity::ensure_registered().await?;
        let mut client = nrd::NrdClient::new(identity)?;
        client.set_account(&stored.user_id, &stored.token);
        Ok(client)
    }

    /// 创建被控房间;业务错误保留错误码(1120 等凭据失效由调用方判定)。
    pub async fn create_host_room(
        &self,
        last_controlled_interval: i64,
    ) -> std::result::Result<nrd::RoomSession, nrd::CallError> {
        self.api()
            .await
            .map_err(nrd::CallError::Transport)?
            .create_host_room(last_controlled_interval)
            .await
    }

    pub async fn set_controllable(&self, controllable: bool) -> Result<()> {
        self.api()
            .await?
            .set_controllable(controllable)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// 会话文件是否仍在(被解绑/登出后变 false)。
    pub fn is_active(&self) -> bool {
        session::load().ok().flatten().is_some()
    }

    /// 账号代际失效:清掉会话文件,下次启动回到扫码登录。
    pub fn retire(&self) {
        session::clear();
    }

    /// 账号凭据被服务端拒绝的业务码(与 uunet 登录侧一致)。
    pub fn invalid_credentials(code: i32) -> bool {
        matches!(code, 1001 | 1002 | 1120)
    }
}
