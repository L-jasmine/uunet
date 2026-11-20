//! 真实后端:UU 远程 NRD API 的扫码登录、会话恢复与设备列表。
//!
//! 端口映射规则仍是本地数据(真实隧道是下一步的工作);规则操作直接
//! 委托给 mock 实现。

use super::identity;
use super::nrd::{self, CallError, Scope};
use super::session;
use super::{Device, QrLoginSession, QrStatus, RestoreOutcome};
use anyhow::{Result, anyhow};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::Duration,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const LONG_POLL_TIMEOUT: Duration = Duration::from_secs(65);

/// 二维码票据缓存:key 是登录 token,值是 (jump_url, 状态查询票据)。
fn tickets() -> &'static Mutex<HashMap<String, (String, String)>> {
    static T: OnceLock<Mutex<HashMap<String, (String, String)>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

fn transport(error: CallError) -> anyhow::Error {
    match error {
        CallError::Transport(e) => e,
        CallError::Business(b) => anyhow!("{b}"),
    }
}

fn saved_session() -> Result<super::session::StoredSession> {
    session::load()?.ok_or_else(|| anyhow!("未登录:没有已保存的会话"))
}

async fn account_client() -> Result<(nrd::NrdClient, super::session::StoredSession)> {
    let stored = saved_session()?;
    let identity = identity::ensure_registered().await?;
    let mut client = nrd::NrdClient::new(identity)?;
    client.set_account(&stored.user_id, &stored.token);
    Ok((client, stored))
}

pub async fn create_qr_login() -> Result<QrLoginSession> {
    let identity = identity::ensure_registered().await?;
    let client = nrd::NrdClient::new(identity)?;
    let qr: nrd::LoginQrCode = client
        .post(
            nrd::LOGIN_QR_GENERATE_PATH,
            Scope::AccountDevice,
            &serde_json::json!({}),
            REQUEST_TIMEOUT,
        )
        .await
        .map_err(transport)?;
    if qr.jump_url.is_empty() || qr.token.is_empty() {
        return Err(anyhow!("二维码响应缺少跳转链接或 token"));
    }
    tickets().lock().unwrap().insert(
        qr.token.clone(),
        (qr.jump_url.clone(), qr.status_ticket().to_owned()),
    );
    Ok(QrLoginSession {
        key: qr.token,
        url: qr.jump_url,
    })
}

pub async fn query_qr_login(key: &str) -> Result<QrStatus> {
    let identity = identity::ensure_registered().await?;
    let (jump_url, ticket) = tickets()
        .lock()
        .unwrap()
        .get(key)
        .cloned()
        .ok_or_else(|| anyhow!("二维码会话不存在"))?;
    let client = nrd::NrdClient::new(identity)?;
    let data: nrd::QrStatusData = client
        .post(
            nrd::LOGIN_QR_STATUS_PATH,
            Scope::AccountDevice,
            &nrd::QrStatusBody {
                jump_url: &jump_url,
                login_status: nrd::QR_PENDING,
                token: &ticket,
            },
            LONG_POLL_TIMEOUT,
        )
        .await
        .map_err(transport)?;
    let status = data
        .resolved()
        .ok_or_else(|| anyhow!("二维码状态响应缺少状态字段"))?;
    match status {
        nrd::QR_PENDING => Ok(QrStatus::WaitingScan),
        nrd::QR_SCANNED => Ok(QrStatus::WaitingConfirm),
        nrd::QR_CANCELED => {
            tickets().lock().unwrap().remove(key);
            Ok(QrStatus::Canceled)
        }
        nrd::QR_CONFIRMED => {
            // 手机端确认后换取正式登录态,并立刻落盘供下次启动恢复。
            let login: nrd::LoginResponse = client
                .post(
                    nrd::LOGIN_BY_QR_PATH,
                    Scope::Device,
                    &nrd::LoginByQrBody {
                        jump_url: &jump_url,
                        token: key,
                    },
                    REQUEST_TIMEOUT,
                )
                .await
                .map_err(transport)?;
            session::save(&login.token, &login.user_id, &login.nickname)?;
            tickets().lock().unwrap().remove(key);
            super::mark_logged_in();
            Ok(QrStatus::Success(login.user_id))
        }
        other => Err(anyhow!("未知二维码状态码 {other}")),
    }
}

pub async fn restore_saved_session() -> RestoreOutcome {
    let stored = match saved_session() {
        Ok(stored) => stored,
        Err(_) => return RestoreOutcome::NoSession,
    };
    let identity = match identity::ensure_registered().await {
        Ok(identity) => identity,
        Err(error) => return RestoreOutcome::Failed(format!("{error:#}")),
    };
    let mut client = match nrd::NrdClient::new(identity) {
        Ok(client) => client,
        Err(error) => return RestoreOutcome::Failed(format!("{error:#}")),
    };
    client.set_account(&stored.user_id, &stored.token);
    match client
        .get::<serde_json::Value>(nrd::USER_INFO_PATH, Scope::AccountDevice)
        .await
    {
        Ok(_) => RestoreOutcome::Restored,
        Err(ref error) if error.invalid_credentials() => {
            session::clear();
            RestoreOutcome::InvalidCredentials
        }
        Err(error) => RestoreOutcome::Failed(format!("{error}")),
    }
}

/// 平台号展示名:官方枚举(omarchyuu DeviceInfo::platform_label)。
/// 注意 1=Windows(桌面端种类号)——官方协议没有 Linux 值,omarchyuu 的
/// Linux 版同样注册为 1;真实系统名走 `os` 字段。
fn platform_label(platform: i32) -> String {
    match platform {
        1 => "Windows".into(),
        2 => "Android".into(),
        3 => "iOS".into(),
        4 => "macOS".into(),
        5 => "TV".into(),
        other => format!("未知平台 ({other})"),
    }
}

fn to_device(info: nrd::DeviceInfo) -> Device {
    // 官方状态值只有 CONNECTED/DISCONNECTED(omarchyuu DeviceInfo::status_label)。
    let online = info.status == "CONNECTED";
    Device {
        id: info.device_id.clone(),
        name: if info.alias.is_empty() {
            info.device_id.clone()
        } else {
            info.alias
        },
        os: if info.os.is_empty() {
            platform_label(info.platform)
        } else {
            info.os
        },
        detail: info.version_name,
        online,
    }
}

pub async fn list_devices() -> Result<Vec<Device>> {
    let (client, _) = account_client().await?;
    let data: nrd::DeviceListData = client
        .get(nrd::DEVICE_LIST_PATH, Scope::AccountDevice)
        .await
        .map_err(transport)?;
    let mut devices = Vec::new();
    if let Some(current) = data.current_device {
        devices.push(to_device(current));
    }
    devices.extend(data.my_binded_devices.into_iter().map(to_device));
    *device_cache().lock().unwrap() = devices.clone();
    Ok(devices)
}

/// 本次进程内拉取过的设备目录,供标题栏按设备号查名字。
fn device_cache() -> &'static Mutex<Vec<Device>> {
    static C: OnceLock<Mutex<Vec<Device>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}

/// 按设备号查已缓存的设备(仅限本进程拉取过列表的)。
pub fn cached_device(id: &str) -> Option<Device> {
    device_cache()
        .lock()
        .unwrap()
        .iter()
        .find(|d| d.id == id)
        .cloned()
}
