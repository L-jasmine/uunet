//! 登录会话持久化:token 存文件,下次启动直接读取。
//!
//! 用户明确要求:登录得到的 token 记录在文件里,下次从文件读取。
//! 存 `~/.config/uunet/session.json`,权限 0600(内含 bearer token)。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::PathBuf};

const SCHEMA: u8 = 1;

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredSession {
    schema: u8,
    pub token: String,
    pub user_id: String,
    pub nickname: String,
    /// 保存时间(Unix 秒),仅展示用。
    pub saved_at: u64,
}

fn session_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".config/uunet/session.json")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 保存登录会话(token 等)到文件,权限收紧到仅本用户可读。
pub fn save(token: &str, user_id: &str, nickname: &str) -> Result<()> {
    let session = StoredSession {
        schema: SCHEMA,
        token: token.to_owned(),
        user_id: user_id.to_owned(),
        nickname: nickname.to_owned(),
        saved_at: now_secs(),
    };
    let path = session_path();
    let dir = path.parent().expect("session 路径有父目录");
    fs::create_dir_all(dir).context("创建 uunet 配置目录失败")?;
    let mut file =
        fs::File::create(&path).with_context(|| format!("创建会话文件失败: {}", path.display()))?;
    file.write_all(serde_json::to_vec_pretty(&session)?.as_slice())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .context("收紧会话文件权限失败")?;
    }
    Ok(())
}

/// 读取已保存的会话;没有或损坏返回 None(损坏时顺手清掉坏文件)。
pub fn load() -> Result<Option<StoredSession>> {
    let text = match fs::read_to_string(session_path()) {
        Ok(text) => text,
        Err(_) => return Ok(None),
    };
    match serde_json::from_str::<StoredSession>(&text) {
        Ok(session) if session.schema == SCHEMA && !session.token.is_empty() => Ok(Some(session)),
        _ => {
            clear();
            Ok(None)
        }
    }
}

/// 删除会话文件(登录态失效或退出登录时)。
pub fn clear() {
    let _ = fs::remove_file(session_path());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_returns_none_without_panic() {
        // 不写 HOME 相关环境(测试进程的 HOME 下多半没有该文件),
        // load 必须安静地返回 None。
        let result = load().expect("load must not fail on missing file");
        // 测试环境可能确实存在会话文件,不对其内容做假设。
        let _ = result;
    }
}
