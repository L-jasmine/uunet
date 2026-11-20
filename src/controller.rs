//! 主控服务:对某台被控设备建立端口映射会话,驱动本地规则监听。
//!
//! 会话生命周期(单设备单会话,由 TUI `c` 键启停):
//! join 房间 → 信令 Controller → connect 帧 → Peer offer/ICE →
//! FILE+PB 就绪 → 按 host.json…(规则文件 `~/.config/uunet/rules/<device>.json`)
//! 中 enabled 的规则启动本地监听。规则文件变化在 500ms 内生效。

use crate::backend::{identity, nrd::{self, CallError}, session};
use crate::forward::{self, Rule};
use crate::peer::controller::ControllerPeer;
use crate::signal::{self, SignalFailure, SignalRole, SignalSession};
use anyhow::{Result, anyhow};
use std::{
    collections::HashMap,
    net::IpAddr,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};
use tokio_util::sync::CancellationToken;

// ---- 规则存取 ----

fn rules_path(device_id: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(format!(".config/uunet/rules/{device_id}.json"))
}

pub(crate) fn load_rules(device_id: &str) -> Vec<Rule> {
    std::fs::read_to_string(rules_path(device_id))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub(crate) fn save_rules(device_id: &str, rules: &[Rule]) -> Result<()> {
    let path = rules_path(device_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(rules)?)?;
    Ok(())
}

pub(crate) fn next_rule_id(rules: &[Rule]) -> u64 {
    rules.iter().map(|r| r.id).max().unwrap_or(0) + 1
}

pub(crate) fn validate_rule(rule: &Rule) -> Result<(), String> {
    if rule.name.trim().is_empty() {
        return Err("名称不能为空".into());
    }
    if rule.local_port == 0 {
        return Err("本地端口应为 1–65535".into());
    }
    if rule.target.is_unspecified() || rule.target.is_multicast() {
        return Err("目标必须为单播 IP 地址".into());
    }
    if rule.remote_port == 0 {
        return Err("目标端口应为 1–65535".into());
    }
    Ok(())
}

// ---- 会话状态(供 TUI 轮询) ----

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MappingStatus {
    pub state: String,
    pub error: Option<String>,
    pub rules_listening: usize,
}

fn statuses() -> &'static Mutex<HashMap<String, MappingStatus>> {
    static S: OnceLock<Mutex<HashMap<String, MappingStatus>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn set_status(device_id: &str, status: MappingStatus) {
    statuses().lock().unwrap().insert(device_id.to_string(), status);
}

pub(crate) fn mapping_status(device_id: &str) -> MappingStatus {
    statuses()
        .lock()
        .unwrap()
        .get(device_id)
        .cloned()
        .unwrap_or_default()
}

fn cancels() -> &'static Mutex<HashMap<String, CancellationToken>> {
    static C: OnceLock<Mutex<HashMap<String, CancellationToken>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 启动某设备的映射会话(已在运行则忽略)。
pub fn connect(device_id: &str) -> Result<()> {
    if crate::backend::mock_mode() {
        return Err(anyhow!("mock 模式不支持建立映射会话"));
    }
    let mut cancels = cancels().lock().unwrap();
    if cancels.contains_key(device_id) {
        return Ok(());
    }
    let cancel = CancellationToken::new();
    cancels.insert(device_id.to_string(), cancel.clone());
    drop(cancels);
    let device = device_id.to_string();
    tokio::spawn(async move {
        set_status(&device, MappingStatus { state: "连接中".into(), ..Default::default() });
        let outcome = run_session(&device, cancel.clone()).await;
        crate::controller::drop_cancel(&device);
        match outcome {
            Ok(()) => set_status(&device, MappingStatus { state: "已断开".into(), ..Default::default() }),
            Err(error) => {
                tracing::error!("主控映射会话失败(target={device}):{error:#}");
                set_status(
                    &device,
                    MappingStatus {
                        state: "失败".into(),
                        error: Some(format!("{error:#}")),
                        rules_listening: 0,
                    },
                );
            }
        }
    });
    Ok(())
}

/// 停止某设备的映射会话。
pub fn disconnect(device_id: &str) {
    if let Some(cancel) = cancels().lock().unwrap().remove(device_id) {
        cancel.cancel();
    }
}

pub(crate) fn drop_cancel(device_id: &str) {
    cancels().lock().unwrap().remove(device_id);
}

pub(crate) fn is_running(device_id: &str) -> bool {
    cancels().lock().unwrap().contains_key(device_id)
}

// ---- 会话主体 ----

async fn run_session(device_id: &str, cancel: CancellationToken) -> Result<()> {
    let stored = session::load()?.ok_or_else(|| anyhow!("未登录:没有已保存的会话"))?;
    let my_device_id = identity::ensure_registered().await?.device_id;
    let mut client = nrd::NrdClient::new(identity::ensure_registered().await?)?;
    client.set_account(&stored.user_id, &stored.token);

    let room = match client.join_device(device_id, false).await {
        Ok(room) => room,
        Err(CallError::Business(ref business)) if business.code == 1136 => {
            return Err(anyhow!("目标设备正被其他主控占用,需要接管(暂未实现)"));
        }
        Err(CallError::Business(ref business)) if HostClient_invalid(business.code) => {
            session::clear();
            return Err(anyhow!("登录态已失效({}),请重新扫码", business.code));
        }
        Err(error) => return Err(anyhow!("{error}")),
    };
    room.validate()?;
    let mut signal_session =
        SignalSession::connect_observed(room, SignalRole::Controller, &cancel, None).await?;

    let frames = crate::pb::build_port_mapping_connect_frames(device_id, 2)?;
    let info = signal_session.start_control(frames).await?;

    let session_cancel = cancel.child_token();
    let (peer, mut candidates) =
        ControllerPeer::new(info.ice_servers_struct(), info.force_relay, session_cancel.clone())
            .await?;
    signal_session
        .negotiate(&info, &peer, &mut candidates)
        .await?;
    tracing::info!(target = %device_id, "controller session negotiated");

    // FILE 通道 + PB 握手就绪(omarchyuu wait_port_mapping_ready 等价)。
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if peer.file_ready() && peer.handshake_connected() {
                return;
            }
            if peer.ended() {
                return; // 外层 timeout 之上无法返回错误,直接结束由超时兜底
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("转发通道或握手未在 15 秒内就绪"));
    tracing::info!(target = %device_id, "controller forwarding ready");

    set_status(device_id, MappingStatus { state: "已连接".into(), ..Default::default() });

    // 规则监听管理:500ms 轮询规则文件,对齐期望集合。
    let transport = peer.transport();
    let mut listeners: HashMap<u64, (Rule, CancellationToken, Arc<Mutex<forward::RuleStatus>>)> =
        HashMap::new();
    let (probe_notify, _probe_watch) = tokio::sync::watch::channel(());
    let mut keep_alive_shutdown = tokio::sync::oneshot::channel();
    let alive = signal_session.keep_alive_controller(
        keep_alive_shutdown.1,
        peer,
        candidates,
        info,
    );
    tokio::pin!(alive);
    let outcome = loop {
        if cancel.is_cancelled() {
            let _ = keep_alive_shutdown.0.send(());
            break Ok(());
        }
        let desired: Vec<Rule> = load_rules(device_id).into_iter().filter(|r| r.enabled).collect();
        // 移除不再期望的监听
        let desired_ids: std::collections::HashSet<u64> =
            desired.iter().map(|r| r.id).collect();
        let stale: Vec<u64> = listeners
            .keys()
            .filter(|id| !desired_ids.contains(id))
            .copied()
            .collect();
        for id in stale {
            if let Some((_, stop, _)) = listeners.remove(&id) {
                stop.cancel();
            }
        }
        // 补齐新期望的监听
        for rule in desired {
            if listeners.contains_key(&rule.id) {
                continue;
            }
            let stop = cancel.child_token();
            let status = Arc::new(Mutex::new(forward::RuleStatus::default()));
            let probe = Arc::new(tokio::sync::Notify::new());
            let probe_for_task = probe.clone();
            let rule_for_task = rule.clone();
            let transport = transport.clone();
            let status_for_task = status.clone();
            let stop_for_task = stop.clone();
            tokio::spawn(async move {
                forward::run_rule(
                    rule_for_task,
                    transport,
                    stop_for_task,
                    status_for_task,
                    probe_for_task,
                )
                .await;
            });
            listeners.insert(rule.id, (rule, stop, status));
            let _ = probe_notify;
            drop(probe);
        }
        // 汇总状态
        let listening = listeners
            .values()
            .filter(|(_, _, status)| lock_status(status).listening)
            .count();
        let error = listeners.values().find_map(|(_, _, status)| {
            lock_status(status).error.clone()
        });
        set_status(
            device_id,
            MappingStatus {
                state: format!("已连接(监听 {listening})"),
                error,
                rules_listening: listening,
            },
        );
        tokio::select! {
            _ = cancel.cancelled() => { let _ = keep_alive_shutdown.0.send(()); break Ok(()); }
            result = &mut alive => {
                break result;
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
        }
    };
    for (_, (_, stop, _)) in listeners.drain() {
        stop.cancel();
    }
    outcome
}

fn HostClient_invalid(code: i32) -> bool {
    matches!(code, 1001 | 1002 | 1120)
}

fn lock_status(status: &Arc<Mutex<forward::RuleStatus>>) -> std::sync::MutexGuard<'_, forward::RuleStatus> {
    status.lock().unwrap_or_else(|e| e.into_inner())
}

/// 供 UI 构造规则时的默认本地地址。
pub(crate) const DEFAULT_LOCAL_ADDR: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
