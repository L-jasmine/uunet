//! 官方 Protobuf 控制信封(omarchyuu `features/stream_control/{wire,publisher}`
//! 的被控最小子集)。
//!
//! oneof 保留完整 tag 范围使未知消息可被 prost 安全跳过;uunet 被控只实现
//! CONTROL 通道上的 ECHO 握手(action 0/1)与端口转发帧(tag 27)。

use anyhow::Result;
use prost::Message as _;
use std::time::{SystemTime, UNIX_EPOCH};

/// omarchyuu `stream_control::ACTION_TYPE_ECHO_REQUEST`。
pub(crate) const ACTION_TYPE_ECHO_REQUEST: i32 = 0;
/// omarchyuu `stream_control::ACTION_TYPE_ECHO_RESPONSE`。
pub(crate) const ACTION_TYPE_ECHO_RESPONSE: i32 = 1;
/// omarchyuu `protocol::official_version::CAPTURE_SETTING_LEVEL`。
const CAPTURE_SETTING_LEVEL: i32 = 6;

/// 被控在 CONTROL 通道 open 时主动发的第一条消息(echo(1, 0, true))。
pub(crate) fn echo_request(seq: i64, timestamp: i64) -> Vec<u8> {
    encode_pb_echo(ACTION_TYPE_ECHO_REQUEST, String::new(), seq, timestamp)
}

/// 对主控 ECHO_REQUEST 的应答;`args` 的 seq 用于主控侧关联。
pub(crate) fn echo_response(sequence: i64, timestamp: i64) -> Vec<u8> {
    encode_pb_echo(
        ACTION_TYPE_ECHO_RESPONSE,
        format!("{{ \"seq\" : {sequence} }}"),
        sequence,
        timestamp,
    )
}

fn encode_pb_echo(action: i32, args: String, seq: i64, timestamp: i64) -> Vec<u8> {
    PbControlMessage {
        seq,
        timestamp,
        payload: Some(PbPayload::SimpleAction(PbSimpleAction {
            action,
            args,
            params: Some(PbSimpleActionParams::FeatureFlag(
                PbFeatureFlag::read_only_viewer(),
            )),
        })),
    }
    .encode_to_vec()
}

pub(crate) fn encode_port_mapping(payload: Vec<u8>) -> Vec<u8> {
    PbControlMessage {
        seq: 0,
        timestamp: 0,
        payload: Some(PbPayload::PortMappingFrame(payload)),
    }
    .encode_to_vec()
}

pub(crate) fn decode_port_mapping(bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    Ok(match PbControlMessage::decode(bytes)?.payload {
        Some(PbPayload::PortMappingFrame(frame)) => Some(frame),
        _ => None,
    })
}

/// 主控 `wait_port_mapping_ready` 以「收到被控 ECHO_RESPONSE」为握手完成,
/// 因此信封必须带全量 tag 集合让未知消息落在 `payload: None`。
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct PbControlMessage {
    #[prost(int64, tag = "1")]
    pub(crate) seq: i64,
    #[prost(int64, tag = "2")]
    pub(crate) timestamp: i64,
    #[prost(
        oneof = "PbPayload",
        tags = "3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28"
    )]
    pub(crate) payload: Option<PbPayload>,
}

/// 官方 main.proto 的完整 oneof tag 范围(main.proto 的 shipped descriptor)。
/// **prost 对 oneof 未声明的 tag 会 panic 而不是跳过**——这正是 omarchyuu
/// 保留全量列表的原因;未实现的业务一律用不透明 bytes 承接,保持互斥语义。
#[derive(Clone, PartialEq, prost::Oneof)]
pub(crate) enum PbPayload {
    #[prost(message, tag = "3")]
    SimpleAction(PbSimpleAction),
    #[prost(bytes, tag = "4")]
    LaunchApp(Vec<u8>),
    #[prost(bytes, tag = "5")]
    ShowApp(Vec<u8>),
    #[prost(bytes, tag = "6")]
    MumuOperate(Vec<u8>),
    #[prost(bytes, tag = "7")]
    Screens(Vec<u8>),
    #[prost(bytes, tag = "8")]
    CaptureChange(Vec<u8>),
    #[prost(bytes, tag = "9")]
    CaptureConfig(Vec<u8>),
    #[prost(bytes, tag = "10")]
    RomMessage(Vec<u8>),
    #[prost(bytes, tag = "11")]
    SendToRom(Vec<u8>),
    #[prost(bytes, tag = "12")]
    ReportError(Vec<u8>),
    #[prost(bytes, tag = "13")]
    SystemMetrics(Vec<u8>),
    #[prost(bytes, tag = "14")]
    ReportQosStats(Vec<u8>),
    #[prost(bytes, tag = "15")]
    SystemStateChange(Vec<u8>),
    #[prost(bytes, tag = "16")]
    QuerySystemState(Vec<u8>),
    #[prost(bytes, tag = "17")]
    ClipboardChange(Vec<u8>),
    #[prost(bytes, tag = "18")]
    CodecNegotiation(Vec<u8>),
    #[prost(bytes, tag = "19")]
    CaptureConfigResponse(Vec<u8>),
    #[prost(bytes, tag = "20")]
    InputEvent(Vec<u8>),
    #[prost(bytes, tag = "21")]
    RpcRequest(Vec<u8>),
    #[prost(bytes, tag = "22")]
    RpcResponse(Vec<u8>),
    #[prost(bytes, tag = "23")]
    ActiveWindowChange(Vec<u8>),
    #[prost(bytes, tag = "24")]
    LaunchCloudPcApp(Vec<u8>),
    #[prost(bytes, tag = "25")]
    CaptureSettingSync(Vec<u8>),
    #[prost(bytes, tag = "26")]
    RemoteDownloadPath(Vec<u8>),
    #[prost(bytes, tag = "27")]
    PortMappingFrame(Vec<u8>),
    #[prost(bytes, tag = "28")]
    TerminalSessionChanged(Vec<u8>),
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct PbSimpleAction {
    #[prost(int32, tag = "1")]
    pub(crate) action: i32,
    #[prost(string, tag = "2")]
    pub(crate) args: String,
    #[prost(oneof = "PbSimpleActionParams", tags = "3, 4, 5")]
    pub(crate) params: Option<PbSimpleActionParams>,
}

#[derive(Clone, PartialEq, prost::Oneof)]
pub(crate) enum PbSimpleActionParams {
    #[prost(bytes, tag = "3")]
    KeyToggle(Vec<u8>),
    #[prost(message, tag = "4")]
    FeatureFlag(PbFeatureFlag),
    #[prost(uint32, tag = "5")]
    Value(u32),
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct PbFeatureFlag {
    #[prost(int32, tag = "1")]
    pub(crate) capture_setting: i32,
    #[prost(int32, tag = "2")]
    pub(crate) simple_action: i32,
    #[prost(int32, tag = "3")]
    pub(crate) system_metrics: i32,
    #[prost(int32, tag = "4")]
    pub(crate) private_screen: i32,
    #[prost(int32, tag = "5")]
    pub(crate) update_acquire: i32,
    #[prost(int32, tag = "6")]
    pub(crate) file_transfer_ftp: i32,
    #[prost(int32, tag = "7")]
    pub(crate) file_transfer_ftp2: i32,
    #[prost(int32, tag = "8")]
    pub(crate) clipboard: i32,
    #[prost(int32, tag = "9")]
    pub(crate) qos_stat: i32,
    #[prost(int32, tag = "10")]
    pub(crate) mumu_control: i32,
    #[prost(int32, tag = "11")]
    pub(crate) virtual_mouse_device: i32,
}

impl PbFeatureFlag {
    /// 只读被控:不采集(qos_stat=1 保留官方默认),文件/剪贴板全关。
    pub(crate) fn read_only_viewer() -> Self {
        Self {
            capture_setting: CAPTURE_SETTING_LEVEL,
            simple_action: 0,
            system_metrics: 0,
            private_screen: 0,
            update_acquire: 0,
            file_transfer_ftp: 2,
            file_transfer_ftp2: 2,
            clipboard: 3,
            qos_stat: 1,
            mumu_control: 0,
            virtual_mouse_device: 0,
        }
    }
}

/// CONTROL 通道入站消息的处理结果(host 视角)。
#[derive(Default, Debug)]
pub(crate) struct Received {
    /// 收到主控 ECHO_RESPONSE(action 1):主控确认了我们的握手。
    pub handshake_complete: bool,
    /// 收到主控 ECHO_REQUEST(action 0):需要回 ECHO_RESPONSE。
    pub handshake_request: bool,
    /// 需要回发的字节(publisher::receive 的 messages)。
    pub messages: Vec<Vec<u8>>,
}

/// 被控 CONTROL 入站:只认 SimpleAction 0/1,其余忽略。
pub(crate) fn receive(bytes: &[u8]) -> Result<Received> {
    // TEXT/JSON 前缀是官方旧协议,数据型会话不会出现;防御性跳过。
    if bytes.first() == Some(&b'{') {
        return Ok(Received::default());
    }
    let msg = PbControlMessage::decode(bytes)?;
    Ok(match msg.payload {
        Some(PbPayload::SimpleAction(action)) if matches!(action.action, 0 | 1) => {
            tracing::debug!(
                action = action.action,
                sequence = msg.seq,
                "host PB handshake received"
            );
            Received {
                handshake_complete: action.action == ACTION_TYPE_ECHO_RESPONSE,
                handshake_request: action.action == ACTION_TYPE_ECHO_REQUEST,
                messages: if action.action == ACTION_TYPE_ECHO_REQUEST {
                    vec![echo_response(msg.seq, msg.timestamp)]
                } else {
                    Vec::new()
                },
            }
        }
        _ => Received::default(),
    })
}

/// `be-controlled` 二进制附件里的主控连接选项(omarchyuu
/// `publisher::ConnectOptions`,字段号原样)。uunet 只读 kind/client_type/
/// connect_type/device_id/features。
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ConnectOptions {
    #[prost(int32, tag = "1")]
    pub kind: i32,
    #[prost(int32, tag = "2")]
    pub screen_id: i32,
    #[prost(message, repeated, tag = "4")]
    pub decoders: Vec<DecoderCapability>,
    #[prost(int32, tag = "8")]
    pub client_type: i32,
    #[prost(string, tag = "9")]
    pub device_id: String,
    #[prost(int32, tag = "10")]
    pub connect_type: i32,
    #[prost(message, optional, tag = "11")]
    pub features: Option<PbFeatureFlag>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DecoderCapability {
    #[prost(int32, tag = "1")]
    pub fps: i32,
    #[prost(int32, tag = "2")]
    pub codec: i32,
    #[prost(int32, tag = "3")]
    pub width: i32,
    #[prost(int32, tag = "4")]
    pub height: i32,
    #[prost(int32, tag = "5")]
    pub chroma: i32,
}

/// 数据型会话 kind:5 文件 / 8 终端 / 9 端口转发(omarchyuu signal/host.rs:347)。
pub(crate) const CONNECT_KIND_FILE: i32 = 5;
pub(crate) const CONNECT_KIND_TERMINAL: i32 = 8;
pub(crate) const CONNECT_KIND_PORTS: i32 = 9;

#[cfg(test)]
mod tests {
    use super::*;

    /// ECHO 请求与应答必须能被信封解出,且 tag 27 的端口帧独立往返。
    #[test]
    fn echo_round_trip() {
        let request = echo_request(1, 0);
        let received = receive(&request).expect("decode echo request");
        assert!(received.handshake_request);
        assert!(!received.handshake_complete);
        assert_eq!(received.messages.len(), 1, "echo request demands a reply");

        let reply = &received.messages[0];
        let received = receive(reply).expect("decode echo response");
        assert!(received.handshake_complete);
        assert!(!received.handshake_request);
        assert!(received.messages.is_empty());
    }

    #[test]
    fn port_mapping_frame_round_trips_through_envelope() {
        let frame = vec![1_u8, 2, 3, 4];
        let envelope = encode_port_mapping(frame.clone());
        let decoded = decode_port_mapping(&envelope).expect("decode").expect("is ports");
        assert_eq!(decoded, frame);
        // 非 tag 27 载荷解出 None。
        assert!(decode_port_mapping(&echo_request(1, 0)).expect("decode").is_none());
    }

    #[test]
    fn unknown_payload_tags_are_skipped() {
        // 手工构造一个 tag 7(截图类)载荷:prost 应忽略并返回默认 Received。
        let mut bytes = vec![0x08, 0x01]; // field 1 varint seq=1
        bytes.extend_from_slice(&[0x3a, 0x02, 0x08, 0x01]); // field 7 (5<<3|2) len 2
        let received = receive(&bytes).expect("decode");
        assert!(!received.handshake_complete);
        assert!(!received.handshake_request);
        assert!(received.messages.is_empty());
    }
}
