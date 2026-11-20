//! 被控拨号白名单:`~/.config/uunet/host.json`(0600)。
//!
//! uunet 被端即转发节点(--host 即允许被控),唯一需要用户管理的是
//! 允许被拨号的目标列表;精确匹配 IP:port:proto,空表 = 全拒。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::{Arc, RwLock},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    /// 允许被拨号的目标白名单(空表 = 全拒)。
    #[serde(default)]
    pub whitelist: Vec<Target>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Target {
    #[serde(default)]
    pub name: String,
    /// "tcp" | "udp";其他值视作 tcp。
    #[serde(default = "default_proto")]
    pub proto: String,
    pub host: String,
    pub port: u16,
}

fn default_proto() -> String {
    "tcp".into()
}

fn settings_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".config/uunet/host.json")
}

pub type Shared = Arc<RwLock<Settings>>;

/// 读取白名单;没有/损坏返回默认值(空表)。
pub fn load() -> Settings {
    fs::read_to_string(settings_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(settings: &Settings) -> Result<()> {
    let path = settings_path();
    let dir = path.parent().expect("host.json 路径有父目录");
    fs::create_dir_all(dir).context("创建 uunet 配置目录失败")?;
    let mut file = fs::File::create(&path)
        .with_context(|| format!("创建白名单文件失败: {}", path.display()))?;
    file.write_all(serde_json::to_vec_pretty(settings)?.as_slice())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .context("收紧白名单文件权限失败")?;
    }
    Ok(())
}

/// 共享句柄:TUI 编辑白名单,转发拨号前实时读取。
pub fn shared() -> Shared {
    static SHARED: std::sync::OnceLock<Shared> = std::sync::OnceLock::new();
    SHARED.get_or_init(|| Arc::new(RwLock::new(load()))).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_toggles_are_ignored() {
        // 旧版 host.json 里的 allow/port_mapping 字段直接忽略,不影响解析。
        let legacy = r#"{"allow":false,"port_mapping":false,"whitelist":[]}"#;
        let settings: Settings = serde_json::from_str(legacy).expect("legacy file parses");
        assert!(settings.whitelist.is_empty());
    }
}
