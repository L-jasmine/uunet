//! NRD(UU 远程互联 API)的最小客户端:请求签名 + 信封解析 + 登录/设备端点。
//!
//! 契约、签名算法与头部集合按 omarchyuu `src/account/api/mod.rs` 移植;
//! 只保留 uunet 用到的端点,不做多端点路由回退。

use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BASE_URL: &str = "https://api.nrd.nie.163.com";
/// 当前官方客户端版本基线(omarchyuu protocol::official_version::CLIENT)。
pub(crate) const CLIENT_VERSION: &str = "4.41.2.2602";
const SIGNING_KEY: &[u8] = b"alWiSzXZTLu3WfFnw13uBru3";
const PACKAGE_NAME: &str = "com.netease.uuremote";
const CHANNEL: &str = "gwqd";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub const DEVICE_INIT_PATH: &str = "/api/v1/device/windows/init";
pub const LOGIN_QR_GENERATE_PATH: &str = "/api/v1/qrcode/gen/login";
pub const LOGIN_QR_STATUS_PATH: &str = "/api/v1/qrcode/login/status";
pub const LOGIN_BY_QR_PATH: &str = "/api/v1/login/by_qrcode";
pub const USER_INFO_PATH: &str = "/api/v1/user/info";
pub const DEVICE_LIST_PATH: &str = "/api/v1/device/list";
pub const ROOM_CREATE_PATH: &str = "/api/v1/room/create";
pub const DEVICE_CONTROLLABLE_PATH: &str = "/api/v1/device/controllable";
pub const ROOM_JOIN_BY_DEVICE_PATH: &str = "/api/v1/room/join/by_device";

/// 已保存登录态失效的业务码(omarchyuu ApiFailure::invalid_saved_credentials)。
const INVALID_CREDENTIAL_CODES: [i32; 3] = [1001, 1002, 1120];

/// 展开错误的完整 source 链(reqwest 顶层只有泛化消息,
/// 超时/连接拒绝/证书等真实原因在链尾)。
fn error_chain(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// 请求的身份范围:决定哪些身份头参与签名。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    /// 仅平台公共头。
    Platform,
    /// 加 client/device/system 三个身份头。
    Device,
    /// 再加 user_id 头与 Bearer 授权。
    AccountDevice,
}

/// 业务层错误(信封 code != 0)。
#[derive(Debug)]
pub struct ApiError {
    pub code: i32,
    pub message: String,
}

impl ApiError {
    pub fn invalid_credentials(&self) -> bool {
        INVALID_CREDENTIAL_CODES.contains(&self.code)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NRD API code {}: {}", self.code, self.message)
    }
}

/// 调用错误:传输层(外层 Result)与业务层(内层)分开。
pub enum CallError {
    Transport(anyhow::Error),
    Business(ApiError),
}

pub type CallResult<T> = Result<T, CallError>;

impl From<ApiError> for CallError {
    fn from(value: ApiError) -> Self {
        CallError::Business(value)
    }
}

impl CallError {
    pub fn invalid_credentials(&self) -> bool {
        matches!(self, CallError::Business(e) if e.invalid_credentials())
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Transport(e) => write!(f, "{e:#}"),
            CallError::Business(e) => write!(f, "{e}"),
        }
    }
}

#[derive(Clone, Deserialize)]
struct Envelope<T> {
    code: i32,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    data: Option<T>,
}

/// 本机身份三元组(进入签名头)。
#[derive(Clone)]
pub struct Identity {
    pub client_id: String,
    pub device_id: String,
    pub system_id: String,
}

#[derive(Clone)]
pub struct NrdClient {
    http: reqwest::Client,
    identity: Identity,
    user_id: Option<String>,
    bearer: Option<String>,
}

impl NrdClient {
    pub fn new(identity: Identity) -> Result<Self> {
        let http = reqwest::Client::builder()
            .https_only(true)
            .http1_only()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .gzip(true)
            .user_agent(concat!("uunet/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("构建 NRD HTTPS 客户端失败")?;
        Ok(Self {
            http,
            identity,
            user_id: None,
            bearer: None,
        })
    }

    /// 登录后设置账号态(user_id 头 + Bearer)。
    pub fn set_account(&mut self, user_id: &str, token: &str) {
        self.user_id = Some(user_id.to_owned());
        self.bearer = Some(format!("Bearer {token}"));
    }

    /// POST 一个 JSON 请求体并解析信封;`body` 须已是紧凑 JSON 字节。
    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        scope: Scope,
        body: &impl Serialize,
        timeout: Duration,
    ) -> CallResult<T> {
        let body = serde_json::to_vec(body)
            .map_err(|e| CallError::Transport(anyhow::anyhow!("请求体 JSON 编码失败: {e}")))?;
        self.send(reqwest::Method::POST, path, scope, body, timeout)
            .await
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str, scope: Scope) -> CallResult<T> {
        self.send(reqwest::Method::GET, path, scope, Vec::new(), REQUEST_TIMEOUT)
            .await
    }

    /// 创建被控房间(被控在线的第一步)。
    /// 从未被控制过的设备官方约定发 -1。
    pub async fn create_host_room(&self, last_controlled_interval: i64) -> CallResult<RoomSession> {
        self.post(
            ROOM_CREATE_PATH,
            Scope::AccountDevice,
            &CreateRoomBody {
                last_controlled_interval,
            },
            REQUEST_TIMEOUT,
        )
        .await
    }

    /// 上报本机可控位(在线时 true,退出时 false)。
    pub async fn set_controllable(&self, controllable: bool) -> CallResult<()> {
        let _: serde_json::Value = self
            .post(
                DEVICE_CONTROLLABLE_PATH,
                Scope::AccountDevice,
                &SetControllableBody { controllable },
                REQUEST_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// 以主控身份加入目标设备的房间(kind 由 connect 帧声明,不在房间 API)。
    pub async fn join_device(&self, device_id: &str, force_join: bool) -> CallResult<RoomSession> {
        if device_id.len() != 16
            || !device_id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(CallError::Transport(anyhow::anyhow!("目标设备号格式无效")));
        }
        self.post(
            &format!("{ROOM_JOIN_BY_DEVICE_PATH}/{device_id}"),
            Scope::AccountDevice,
            &serde_json::json!({ "force_join": force_join }),
            REQUEST_TIMEOUT,
        )
        .await
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        scope: Scope,
        body: Vec<u8>,
        timeout: Duration,
    ) -> CallResult<T> {
        let headers = self.signed_headers(method.as_str(), path, &body, scope);
        let mut request = self
            .http
            .request(method.clone(), format!("{BASE_URL}{path}"))
            .headers(headers)
            .timeout(timeout);
        if !body.is_empty() {
            request = request.body(body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| CallError::Transport(anyhow::anyhow!("NRD 请求失败: {}", error_chain(&e))))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| CallError::Transport(anyhow::anyhow!("NRD 响应读取失败: {}", error_chain(&e))))?;
        let envelope: Envelope<serde_json::Value> = serde_json::from_slice(&bytes)
            .map_err(|e| CallError::Transport(anyhow::anyhow!("NRD 响应解析失败 (HTTP {status}): {e}")))?;
        // 与 omarchyuu 一致:非 200 且信封 code==0 时,以 HTTP 状态码为准。
        let code = if status != reqwest::StatusCode::OK && envelope.code == 0 {
            i32::from(status.as_u16())
        } else {
            envelope.code
        };
        if code != 0 {
            return Err(CallError::Business(ApiError {
                code,
                message: envelope.msg,
            }));
        }
        let data = envelope
            .data
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                CallError::Transport(anyhow::anyhow!("NRD 成功数据解码失败 ({path}): {e}"))
            })?
            .ok_or_else(|| {
                CallError::Transport(anyhow::anyhow!("NRD 响应缺少 data ({path})"))
            })?;
        Ok(data)
    }

    /// 全部 x-param 头(除 sign)按名排序后 `k=v` 以 `&` 连接,与
    /// METHOD + path + body 一起做 HMAC-SHA256。
    fn signed_headers(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        scope: Scope,
    ) -> reqwest::header::HeaderMap {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut pairs: Vec<(String, String)> = vec![
            ("x-param-plat".into(), "1".into()),
            ("x-param-vn".into(), CLIENT_VERSION.into()),
            (
                "x-param-vc".into(),
                CLIENT_VERSION.rsplit('.').next().unwrap_or_default().into(),
            ),
            ("x-param-pkgn".into(), PACKAGE_NAME.into()),
            ("x-param-chn".into(), CHANNEL.into()),
            ("x-param-lang".into(), "zh-CN".into()),
            ("x-param-cnt".into(), "zh-CN".into()),
            ("x-param-rel".into(), "prod".into()),
            ("x-param-opr".into(), String::new()),
            ("x-param-ts".into(), timestamp.to_string()),
        ];
        if scope != Scope::Platform {
            pairs.push(("x-param-client-id".into(), self.identity.client_id.clone()));
            pairs.push(("x-param-device-id".into(), self.identity.device_id.clone()));
            pairs.push(("x-param-system-id".into(), self.identity.system_id.clone()));
        }
        if scope == Scope::AccountDevice
            && let Some(user_id) = &self.user_id
        {
            pairs.push(("x-param-user-id".into(), user_id.clone()));
        }
        pairs.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        let canonical = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(SIGNING_KEY)
            .expect("HMAC 接受任意长度密钥");
        mac.update(method.to_ascii_uppercase().as_bytes());
        mac.update(path.as_bytes());
        mac.update(canonical.as_bytes());
        mac.update(body);
        let signature = hex_lower(&mac.finalize().into_bytes());

        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in &pairs {
            if let Ok(header_name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) {
                if let Ok(header_value) = reqwest::header::HeaderValue::from_str(value) {
                    headers.insert(header_name, header_value);
                }
            }
        }
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&signature) {
            headers.insert("x-param-sign", value);
        }
        if scope == Scope::AccountDevice
            && let Some(bearer) = &self.bearer
            && let Ok(value) = reqwest::header::HeaderValue::from_str(bearer)
        {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        headers
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

// ---- 登录/设备端点的请求与响应结构 ----

#[derive(Deserialize)]
pub struct LoginQrCode {
    #[serde(rename = "qrcode_jump_url")]
    pub jump_url: String,
    pub token: String,
    #[serde(default)]
    pub status_query_ticket: Option<String>,
}

impl LoginQrCode {
    /// 状态查询票据:服务端给了专用 ticket 就用它,否则用登录 token。
    pub fn status_ticket(&self) -> &str {
        self.status_query_ticket.as_deref().unwrap_or(&self.token)
    }
}

#[derive(Serialize)]
pub struct QrStatusBody<'a> {
    #[serde(rename = "qrcode_jump_url")]
    pub jump_url: &'a str,
    pub login_status: i32,
    pub token: &'a str,
}

#[derive(Deserialize)]
pub struct QrStatusData {
    #[serde(default)]
    pub status: Option<i32>,
    #[serde(default)]
    pub login_status: Option<i32>,
}

impl QrStatusData {
    pub fn resolved(self) -> Option<i32> {
        self.status.or(self.login_status)
    }
}

#[derive(Serialize)]
pub struct LoginByQrBody<'a> {
    #[serde(rename = "qrcode_jump_url")]
    pub jump_url: &'a str,
    pub token: &'a str,
}

#[derive(Clone, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub user_id: String,
    #[serde(default)]
    pub nickname: String,
}

#[derive(Deserialize)]
pub struct DeviceInitData {
    pub device_id: String,
    /// 服务端返回的设备别名;uunet 暂不使用,保留解析以防未来展示。
    #[serde(default)]
    #[allow(dead_code)]
    pub alias: String,
}

#[derive(Clone, Deserialize)]
pub struct DeviceInfo {
    pub device_id: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub platform: i32,
    /// 设备注册时上报的操作系统名(如 "Omarchy Linux 25.x")。官方协议的
    /// platform 枚举没有 Linux 值(1=Windows/桌面端种类号),Linux 设备的
    /// 真实系统名靠这个字段展示。
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub version_name: String,
}

#[derive(Deserialize)]
pub struct DeviceListData {
    #[serde(default)]
    pub current_device: Option<DeviceInfo>,
    #[serde(default)]
    pub my_binded_devices: Vec<DeviceInfo>,
}

/// 被控房间会话:信令 WebSocket 的端点与鉴权都从这里来
/// (字段集对应 omarchyuu `account::api::RoomSession`)。
#[derive(Clone, Deserialize)]
pub struct RoomSession {
    pub token: String,
    #[serde(default)]
    pub ws_connect_timeout_ms: i32,
    #[serde(default)]
    pub streamer_retry_delta_ms: i32,
    #[serde(default)]
    pub max_reconnect_delta: i32,
    #[serde(default)]
    pub signaling_list: Vec<String>,
    #[serde(default)]
    pub signaling_server: String,
}

impl RoomSession {
    /// 房间鉴权 token(进信令握手头)。
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.token.is_empty() {
            anyhow::bail!("房间响应没有鉴权 token");
        }
        if self.signaling_list.iter().all(String::is_empty) && self.signaling_server.is_empty() {
            anyhow::bail!("房间响应没有信令服务器");
        }
        Ok(())
    }

    /// 信令端点列表;list 优先,为空则退回单个 server 字段。
    pub fn signaling_endpoints(&self) -> Vec<&str> {
        let endpoints = self
            .signaling_list
            .iter()
            .map(String::as_str)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if endpoints.is_empty() && !self.signaling_server.is_empty() {
            vec![self.signaling_server.as_str()]
        } else {
            endpoints
        }
    }
}

#[derive(Serialize)]
struct CreateRoomBody {
    last_controlled_interval: i64,
}

#[derive(Serialize)]
struct SetControllableBody {
    controllable: bool,
}

/// 二维码状态码(omarchyuu login/mod.rs)。
pub const QR_PENDING: i32 = 1;
pub const QR_SCANNED: i32 = 2;
pub const QR_CANCELED: i32 = 3;
pub const QR_CONFIRMED: i32 = 4;
