//! 本机被控允许策略:`~/.config/uunet/host.json`(0600)。
//!
//! 对应 omarchyuu 被控设置里的 allow / port_mapping 两个开关;
//! 白名单是 uunet 自己的收紧项:被控拨号只允许列表内目标,空表全拒。

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
    /// 总开关:是否允许被控在线。
    #[serde(default)]
    pub allow: bool,
    /// 是否允许主控对本机做端口转发。
    #[serde(default)]
    pub port_mapping: bool,
    /// 允许被拨号的 target 白名单(空表 = 全拒)。
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

/// 读取策略文件;没有/损坏返回默认值(全关)。
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
        .with_context(|| format!("创建策略文件失败: {}", path.display()))?;
    file.write_all(serde_json::to_vec_pretty(settings)?.as_slice())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .context("收紧策略文件权限失败")?;
    }
    Ok(())
}

/// 共享句柄:TUI 改开关,被控转发拨号前读白名单。
pub fn shared() -> Shared {
    static SHARED: std::sync::OnceLock<Shared> = std::sync::OnceLock::new();
    SHARED.get_or_init(|| Arc::new(RwLock::new(load()))).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_fully_closed() {
        let settings = Settings::default();
        assert!(!settings.allow);
        assert!(!settings.port_mapping);
        assert!(settings.whitelist.is_empty());
    }

    #[test]
    fn proto_defaults_to_tcp() {
        let target: Target = serde_json::from_str(r#"{"host":"127.0.0.1","port":22}"#).unwrap();
        assert_eq!(target.proto, "tcp");
    }
}
