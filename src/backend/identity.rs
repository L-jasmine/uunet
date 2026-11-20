//! 本机设备身份:首次运行生成并注册,持久化到 identity.json。
//!
//! 身份结构与注册流程对应 omarchyuu 的 `NativeIdentity` + device init:
//! client_id 是本机生成的 UUID,device_id 由服务端在设备注册时签发,
//! system_id 是安装级稳定标识(/etc/machine-id 派生的 UUID v5)。

use super::nrd::{self, Scope};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    sync::OnceLock,
};

const SCHEMA: u8 = 1;

#[derive(Clone, Serialize, Deserialize)]
pub struct IdentityFile {
    schema: u8,
    client_id: String,
    pub device_id: String,
    pub system_id: String,
    pub machine_guid: String,
}

impl IdentityFile {
    pub fn client_identity(&self) -> nrd::Identity {
        nrd::Identity {
            client_id: self.client_id.clone(),
            device_id: self.device_id.clone(),
            system_id: self.system_id.clone(),
        }
    }
}

fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".config/uunet")
}

fn identity_path() -> PathBuf {
    config_dir().join("identity.json")
}

/// /etc/machine-id(或 dbus 的),失败则返回 None。
fn machine_id() -> Option<String> {
    for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(text) = fs::read_to_string(path) {
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

fn hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "uunet-host".into())
}

/// 生成本机身份。machine_guid 取 machine-id;system_id 由其派生 UUID v5,
/// 保证同一台机器多次重装身份文件后 system_id 仍一致。
fn generate() -> Result<IdentityFile> {
    let machine_guid = machine_id().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let namespace = uuid::Uuid::parse_str("6c1f3a1e-4d0e-4f4a-9a7b-2f7e5c3d1b00")
        .expect("内置命名空间是合法 UUID");
    let system_id = uuid::Uuid::new_v5(&namespace, machine_guid.as_bytes()).to_string();
    Ok(IdentityFile {
        schema: SCHEMA,
        client_id: uuid::Uuid::new_v4().to_string(),
        device_id: String::new(),
        system_id,
        machine_guid,
    })
}

/// 读取身份文件;没有就生成并写盘。
fn load_or_create() -> Result<IdentityFile> {
    let path = identity_path();
    if let Ok(text) = fs::read_to_string(&path) {
        let identity: IdentityFile = serde_json::from_str(&text)
            .with_context(|| format!("身份文件损坏: {}", path.display()))?;
        if identity.schema != SCHEMA {
            bail!("身份文件版本不认识,请删除 {} 重新生成", path.display());
        }
        return Ok(identity);
    }
    let identity = generate()?;
    fs::create_dir_all(config_dir()).context("创建 uunet 配置目录失败")?;
    fs::write(&path, serde_json::to_vec_pretty(&identity)?)
        .with_context(|| format!("写身份文件失败: {}", path.display()))?;
    Ok(identity)
}

fn save(identity: &IdentityFile) -> Result<()> {
    fs::create_dir_all(config_dir())?;
    fs::write(identity_path(), serde_json::to_vec_pretty(identity)?)?;
    Ok(())
}

fn read_trimmed(path: &str) -> String {
    fs::read_to_string(path)
        .map(|t| t.trim().to_string())
        .unwrap_or_default()
}

fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default()
}

fn primary_mac() -> String {
    if let Ok(entries) = fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "lo" {
                continue;
            }
            let mac = read_trimmed(&format!("{}/address", entry.path().display()));
            if !mac.is_empty() && mac != "00:00:00:00:00:00" {
                return mac;
            }
        }
    }
    String::new()
}

fn memory_bytes() -> i64 {
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|l| {
                l.starts_with("MemTotal")
                    .then(|| {
                        l.split_whitespace()
                            .nth(1)
                            .and_then(|kb| kb.parse::<i64>().ok())
                    })
                    .flatten()
            })
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

fn os_name() -> String {
    fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|l| {
                l.starts_with("PRETTY_NAME=")
                    .then(|| l.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| "Linux".into())
}

/// 设备注册请求体。字段名与 Windows 端契约一致,Linux 下尽力采集。
fn init_body(identity: &IdentityFile) -> serde_json::Value {
    let board = {
        let vendor = read_trimmed("/sys/class/dmi/id/board_vendor");
        let name = read_trimmed("/sys/class/dmi/id/board_name");
        if vendor.is_empty() {
            name
        } else {
            format!("{vendor} {name}")
        }
        .trim()
        .to_string()
    };
    let host = hostname();
    let name = if host == "uunet-host" {
        format!("UU-{}", &identity.client_id[..8].replace('-', ""))
    } else {
        host
    };
    serde_json::json!({
        "name": name,
        "client_id": identity.client_id,
        "system_id": identity.system_id,
        "machine_guid": identity.machine_guid,
        "os": os_name(),
        "base_board": board,
        "cpu": cpu_model(),
        "video": [],
        "mac": primary_mac(),
        "memory": memory_bytes(),
        "screen": "",
        "platform": 1,
        "controllable": false,
    })
}

static REGISTERED: OnceLock<nrd::Identity> = OnceLock::new();

/// 取本机身份;未注册(device_id 为空)时向服务端注册并持久化。
/// 进程内只做一次,之后命中缓存。
pub async fn ensure_registered() -> Result<nrd::Identity> {
    if let Some(identity) = REGISTERED.get() {
        return Ok(identity.clone());
    }
    let mut identity = load_or_create()?;
    if identity.device_id.is_empty() {
        let client = nrd::NrdClient::new(identity.client_identity())?;
        let body = init_body(&identity);
        let data: nrd::DeviceInitData = client
            .post(nrd::DEVICE_INIT_PATH, Scope::Platform, &body, std::time::Duration::from_secs(30))
            .await
            .map_err(|e| anyhow::anyhow!("设备注册失败: {e}"))?;
        if data.device_id.len() != 16 {
            bail!("设备注册返回的 device_id 格式无效");
        }
        identity.device_id = data.device_id;
        save(&identity)?;
    }
    let client_identity = identity.client_identity();
    let _ = REGISTERED.set(client_identity.clone());
    Ok(client_identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_is_stable_for_same_machine_id() {
        // 同一 machine-id 派生的 system_id 必须一致;client_id 必须不同。
        let machine = machine_id();
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_ne!(a.client_id, b.client_id);
        if machine.is_some() {
            assert_eq!(a.system_id, b.system_id);
            assert_eq!(a.machine_guid, b.machine_guid);
        }
        assert_eq!(a.schema, SCHEMA);
        assert!(a.device_id.is_empty());
    }

    #[test]
    fn hostname_has_fallback() {
        assert!(!hostname().is_empty());
    }
}
