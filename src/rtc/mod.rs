//! WebRTC 连接内核(uunet 被控侧):SDP/ICE 协商与连接所有权。
//!
//! 拷自 omarchyuu `transport/rtc`;NativePeer/媒体收发/FEC 恢复等主控
//! 专用部分不在此,被控直接使用 `ConnectionCore`。

pub(crate) mod core;
pub(crate) mod negotiation;

pub(crate) use core::ConnectionCore;
pub(crate) use negotiation::{
    MAX_DATA_CHANNEL_MESSAGE_SIZE, apply_uu_application_attributes_for_role,
    negotiated_mixed_kcp_version,
};
