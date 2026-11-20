//! Mock backend: QR sessions advance on a timer (scan → confirm → success),
//! devices and rules live in process memory. Enough to drive the whole UI
//! offline (`--mock`).

use super::{Device, Protocol, QrLoginSession, QrStatus, Rule, RuleForm};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const SCAN_WINDOW: Duration = Duration::from_secs(6);
const CONFIRM_WINDOW: Duration = Duration::from_secs(12);
const EXPIRE_WINDOW: Duration = Duration::from_secs(60);

fn sessions() -> &'static Mutex<HashMap<String, Instant>> {
    static S: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn devices() -> &'static Mutex<Vec<Device>> {
    static D: OnceLock<Mutex<Vec<Device>>> = OnceLock::new();
    D.get_or_init(|| {
        Mutex::new(vec![
            Device {
                id: "nas0000000000000".into(),
                name: "office-nas".into(),
                os: "Linux".into(),
                detail: "mock 0.1.0".into(),
                online: true,
            },
            Device {
                id: "pc00000000000000".into(),
                name: "living-room-pc".into(),
                os: "Windows".into(),
                detail: "mock 0.1.0".into(),
                online: true,
            },
            Device {
                id: "lab00000000000000".into(),
                name: "lab-server".into(),
                os: "Linux".into(),
                detail: "mock 0.1.0".into(),
                online: false,
            },
        ])
    })
}

/// Rules are stored per device; ids are unique across the whole network.
#[allow(dead_code)]
fn rules() -> &'static Mutex<HashMap<String, Vec<Rule>>> {
    static R: OnceLock<Mutex<HashMap<String, Vec<Rule>>>> = OnceLock::new();
    R.get_or_init(|| {
        Mutex::new(HashMap::from([(
            "nas0000000000000".to_string(),
            vec![
                Rule {
                    id: 1,
                    name: "NAS Web".into(),
                    protocol: Protocol::Tcp,
                    local_port: 8080,
                    target: "10.0.0.2".into(),
                    target_port: 5000,
                    enabled: true,
                },
                Rule {
                    id: 2,
                    name: "SSH".into(),
                    protocol: Protocol::Tcp,
                    local_port: 2222,
                    target: "10.0.0.2".into(),
                    target_port: 22,
                    enabled: false,
                },
            ],
        )]))
    })
}

pub fn device_by_id(id: &str) -> Option<Device> {
    devices()
        .lock()
        .unwrap()
        .iter()
        .find(|d| d.id == id)
        .cloned()
}

pub async fn create_qr_login() -> anyhow::Result<QrLoginSession> {
    let key = format!(
        "mock-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let url = format!("https://uunet.example/qr/{key}");
    sessions()
        .lock()
        .unwrap()
        .insert(key.clone(), Instant::now());
    Ok(QrLoginSession { key, url })
}

pub async fn query_qr_login(key: &str) -> anyhow::Result<QrStatus> {
    let created = sessions()
        .lock()
        .unwrap()
        .get(key)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("登录会话不存在"))?;
    let elapsed = created.elapsed();
    if elapsed >= EXPIRE_WINDOW {
        sessions().lock().unwrap().remove(key);
        return Ok(QrStatus::Expired);
    }
    Ok(match elapsed {
        e if e < SCAN_WINDOW => QrStatus::WaitingScan,
        e if e < CONFIRM_WINDOW => QrStatus::WaitingConfirm,
        _ => QrStatus::Success(format!("token-{key}")),
    })
}

pub async fn list_devices() -> anyhow::Result<Vec<Device>> {
    Ok(devices().lock().unwrap().clone())
}

// 规则 API 已被 controller 的真实持久化取代,mock 侧保留供回退使用。
#[allow(dead_code)]
pub fn list_rules(device_id: &str) -> Vec<Rule> {
    rules()
        .lock()
        .unwrap()
        .get(device_id)
        .cloned()
        .unwrap_or_default()
}

/// Validates the form and stores a new rule on the device; the error
/// message goes straight into the UI's form error line.
#[allow(dead_code)]
pub fn add_rule(device_id: &str, form: &RuleForm) -> Result<Rule, String> {
    validate(form)?;
    let mut store = rules().lock().unwrap();
    let id = store.values().flatten().map(|r| r.id).max().unwrap_or(0) + 1;
    let rule = Rule {
        id,
        name: form.name.trim().to_string(),
        protocol: form.protocol,
        local_port: form.local_port,
        target: form.target.trim().to_string(),
        target_port: form.target_port,
        enabled: true,
    };
    store
        .entry(device_id.to_string())
        .or_default()
        .push(rule.clone());
    Ok(rule)
}

#[allow(dead_code)]
pub fn toggle_rule(id: u64) {
    let mut store = rules().lock().unwrap();
    for list in store.values_mut() {
        if let Some(rule) = list.iter_mut().find(|r| r.id == id) {
            rule.enabled = !rule.enabled;
            break;
        }
    }
}

#[allow(dead_code)]
pub fn delete_rule(id: u64) {
    rules().lock().unwrap().values_mut().for_each(|list| {
        list.retain(|r| r.id != id);
    });
}

#[allow(dead_code)]
fn validate(form: &RuleForm) -> Result<(), String> {
    if form.name.trim().is_empty() {
        return Err("名称不能为空".into());
    }
    if form.local_port == 0 {
        return Err("本地端口应为 1–65535".into());
    }
    if form.target.trim().is_empty() {
        return Err("目标地址不能为空".into());
    }
    if form.target_port == 0 {
        return Err("目标端口应为 1–65535".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> RuleForm {
        RuleForm {
            name: "测试".into(),
            protocol: Protocol::Tcp,
            local_port: 8080,
            target: "10.0.0.2".into(),
            target_port: 80,
        }
    }

    #[test]
    fn add_rejects_empty_name_and_zero_ports() {
        let mut f = form();
        f.name = "  ".into();
        assert!(add_rule("nas0000000000000", &f).is_err());
        f = form();
        f.local_port = 0;
        assert!(add_rule("nas0000000000000", &f).is_err());
        f = form();
        f.target_port = 0;
        assert!(add_rule("nas0000000000000", &f).is_err());
    }

    #[test]
    fn qr_session_expires_and_is_removed() {
        let session = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(create_qr_login())
            .expect("create");
        sessions()
            .lock()
            .unwrap()
            .insert(session.key.clone(), Instant::now() - EXPIRE_WINDOW);
        let status = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(query_qr_login(&session.key))
            .expect("query");
        assert_eq!(status, QrStatus::Expired);
        assert!(
            !sessions().lock().unwrap().contains_key(&session.key),
            "expired session must be dropped"
        );
    }

    #[test]
    fn add_assigns_increasing_ids_across_devices() {
        let a = add_rule("nas0000000000000", &form()).expect("valid form");
        let b = add_rule("pc00000000000000", &form()).expect("valid form");
        // ids are unique network-wide, even across devices
        assert_eq!(b.id, a.id + 1);
        assert!(list_rules("nas0000000000000").iter().all(|r| r.id != b.id));
        assert!(list_rules("pc00000000000000").iter().any(|r| r.id == b.id));
        delete_rule(b.id);
        assert!(!list_rules("pc00000000000000").iter().any(|r| r.id == b.id));
    }

    #[test]
    fn toggle_flips_enabled_on_stored_rule() {
        let rule = add_rule("nas0000000000000", &form()).expect("valid form");
        toggle_rule(rule.id);
        let stored = list_rules("nas0000000000000")
            .into_iter()
            .find(|r| r.id == rule.id)
            .expect("rule kept");
        assert!(!stored.enabled);
        delete_rule(rule.id);
    }

    #[test]
    fn device_catalog_has_online_and_offline_hosts() {
        let all = devices().lock().unwrap().clone();
        assert!(all.len() >= 2);
        assert!(all.iter().any(|d| d.online) && all.iter().any(|d| !d.online));
        assert!(device_by_id(&all[0].id).is_some());
    }
}
