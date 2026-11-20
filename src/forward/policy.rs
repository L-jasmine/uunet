//! 被控拨号白名单:omarchyuu 没有的 uunet 收紧项。
//!
//! 每次拨号前实时读取 host.json;精确匹配 host:port:proto,
//! 空白名单 = 全部拒绝。

use crate::host::settings;
use std::net::IpAddr;

pub(crate) fn allow(host: IpAddr, port: u16, protocol: &super::Protocol) -> bool {
    let settings = settings::shared();
    let settings = settings.read().unwrap();
    let proto = match protocol {
        super::Protocol::Tcp => "tcp",
        super::Protocol::Udp => "udp",
    };
    settings.whitelist.iter().any(|target| {
        target.port == port
            && target.proto.trim().to_ascii_lowercase() == proto
            && parse_host(&target.host).is_some_and(|allowed| allowed == host)
    })
}

/// 白名单条目的 host 支持直接 IP;域名解析留待后续(避免拨号路径上的 DNS)。
fn parse_host(text: &str) -> Option<IpAddr> {
    text.trim().parse::<IpAddr>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_names() {
        assert!(parse_host("127.0.0.1").is_some());
        assert!(parse_host("::1").is_some());
        assert!(parse_host("nas.local").is_none());
    }
}
