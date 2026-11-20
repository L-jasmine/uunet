//! Backend seam: QR-code login, session restore, device list and port-map
//! rules.
//!
//! The UI layer only talks to the re-exported functions here; they dispatch
//! to `mock` (offline) or `uu` (real NRD API) according to the `--mock`
//! flag set at startup. Port-map rules are local data in both modes — the
//! real tunnel is a separate workstream — so rule operations stay
//! synchronous: the UI calls them from its key-event handler, which cannot
//! await.

pub mod identity;
pub mod mock;
pub mod nrd;
pub mod session;
pub mod uu;

use std::sync::OnceLock;

/// One QR login session: the URL to encode into the QR image plus the key
/// used to poll its status.
#[derive(Clone, Debug)]
pub struct QrLoginSession {
    pub key: String,
    pub url: String,
}

/// Server-side state of a QR login session.
#[derive(Clone, Debug, PartialEq)]
pub enum QrStatus {
    /// QR generated, nobody scanned it yet.
    WaitingScan,
    /// Scanned; the user still has to confirm on the phone.
    WaitingConfirm,
    /// The phone backed out; the QR must be regenerated.
    Canceled,
    /// Confirmed; carries the user id (the session token is already saved
    /// to the session file by the backend).
    Success(String),
    /// Nobody confirmed within the window; the QR must be regenerated.
    Expired,
}

/// Outcome of validating the session file at startup.
#[derive(Clone, Debug, PartialEq)]
pub enum RestoreOutcome {
    /// No session file — needs a fresh QR login.
    NoSession,
    /// Saved token is still valid — skip login entirely.
    Restored,
    /// Server rejected the saved token — file was cleared, do a fresh login.
    InvalidCredentials,
    /// Could not check (network, registration); keep the file and fall back
    /// to the login page so the user can decide.
    Failed(String),
}

/// A device in the account's network (tailnet 意义上的 machines)。
#[derive(Clone, Debug)]
pub struct Device {
    /// 服务端签发的 16 位设备号。
    pub id: String,
    pub name: String,
    pub os: String,
    /// 附加信息(客户端版本号等)。
    pub detail: String,
    pub online: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[allow(dead_code)]
impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Tcp => "TCP",
            Protocol::Udp => "UDP",
        }
    }
}

/// A forwarding rule for one device: the controller listens locally and
/// every connection is tunneled to `target:target_port` on that device.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Rule {
    pub id: u64,
    pub name: String,
    pub protocol: Protocol,
    pub local_port: u16,
    pub target: String,
    pub target_port: u16,
    pub enabled: bool,
}

#[allow(dead_code)]
impl Rule {
    pub fn listen_label(&self) -> String {
        format!(
            ":{}{}",
            self.local_port,
            match self.protocol {
                Protocol::Tcp => "/tcp",
                Protocol::Udp => "/udp",
            }
        )
    }

    pub fn target_label(&self) -> String {
        format!("{}:{}", self.target, self.target_port)
    }

    /// One-line status for the rules list.
    pub fn status_label(&self) -> &'static str {
        if self.enabled {
            "运行中"
        } else {
            "已停用"
        }
    }
}

/// Field-level validation input for `add_rule`.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct RuleForm {
    pub name: String,
    pub protocol: Protocol,
    pub local_port: u16,
    pub target: String,
    pub target_port: u16,
}

// ---- 模式选择 ----

static MOCK: OnceLock<bool> = OnceLock::new();

/// 登录态建立信号:被控服务(--host)等它再启动。
/// 读写共用同一通道——此前两个函数各自持有一份 static,标记发进了
/// 无人监听的通道,被控服务永远不会启动。
fn logged_in_signal() -> &'static tokio::sync::watch::Sender<bool> {
    static SIGNAL: OnceLock<tokio::sync::watch::Sender<bool>> = OnceLock::new();
    SIGNAL.get_or_init(|| tokio::sync::watch::channel(false).0)
}

pub fn logged_in() -> tokio::sync::watch::Receiver<bool> {
    logged_in_signal().subscribe()
}

pub(crate) fn mark_logged_in() {
    logged_in_signal().send_replace(true);
}

/// 启动时设置后端模式;`--mock` 走离线实现。
pub fn init(mock: bool) {
    let _ = MOCK.set(mock);
}

pub fn mock_mode() -> bool {
    *MOCK.get().unwrap_or(&false)
}

// ---- 后端能力(按模式分发) ----

pub async fn create_qr_login() -> anyhow::Result<QrLoginSession> {
    if mock_mode() {
        mock::create_qr_login().await
    } else {
        uu::create_qr_login().await
    }
}

pub async fn query_qr_login(key: &str) -> anyhow::Result<QrStatus> {
    if mock_mode() {
        mock::query_qr_login(key).await
    } else {
        uu::query_qr_login(key).await
    }
}

/// 启动时验证已保存的登录态(token 文件)。
pub async fn restore_saved_session() -> RestoreOutcome {
    if mock_mode() {
        RestoreOutcome::NoSession
    } else {
        let outcome = uu::restore_saved_session().await;
        if outcome == RestoreOutcome::Restored {
            mark_logged_in();
        }
        outcome
    }
}

pub async fn list_devices() -> anyhow::Result<Vec<Device>> {
    if mock_mode() {
        mock::list_devices().await
    } else {
        uu::list_devices().await
    }
}

pub fn device_by_id(id: &str) -> Option<Device> {
    if mock_mode() {
        mock::device_by_id(id)
    } else {
        uu::cached_device(id)
    }
}

// ---- 端口映射规则(两种模式同为本地数据) ----
// 已被 controller 的真实规则持久化取代,保留供 mock / 后续回退使用。
#[allow(dead_code)]
pub fn list_rules(device_id: &str) -> Vec<Rule> {
    mock::list_rules(device_id)
}

#[allow(dead_code)]
pub fn add_rule(device_id: &str, form: &RuleForm) -> Result<Rule, String> {
    mock::add_rule(device_id, form)
}

#[allow(dead_code)]
pub fn toggle_rule(id: u64) {
    mock::toggle_rule(id)
}

#[allow(dead_code)]
pub fn delete_rule(id: u64) {
    mock::delete_rule(id)
}
