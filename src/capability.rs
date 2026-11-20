//! UU device_capability 结构(omarchyuu `protocol/capability.rs` 子集)。
//!
//! 只保留被控端 `be-controlled` 解析与 `device_capability` 上报所需的类型;
//! 主控侧的 `DualCapability` 交集协商不在 uunet 的被控路径上。

use serde::{Deserialize, Serialize};

/// UU SDK VideoCodec::AV1。
pub(crate) const AV1_CODEC_ID: i32 = 5;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct CodecCapability {
    pub video_codec: i32,
    pub width: i32,
    pub height: i32,
    pub chroma_sampling: u8,
    pub bit_depth: u8,
    pub codec_impl: i32,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct DisplayCapability {
    pub id: i32,
    pub fps: u32,
    #[serde(rename = "type")]
    pub kind: i32,
    pub hdr: i32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct DeviceCapability {
    #[serde(
        default,
        rename = "openuuyc_preferred_codec",
        skip_serializing_if = "Option::is_none"
    )]
    pub preferred_codec: Option<i32>,
    /// 解码与展示各自独立支持 SDR10(与 HDR 无关)。
    pub openuuyc_sdr_10bit: bool,
    pub ice_id: String,
    pub display_info: Vec<DisplayCapability>,
    pub video_codec_capability: Vec<CodecCapability>,
}
