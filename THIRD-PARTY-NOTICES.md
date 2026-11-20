# Third-Party Notices

This file records the principal third-party sources in this repository and
their licensing boundaries. It does not relicense any third-party material.

## Ported application code

- **OpenUUYC** (https://github.com/djkcyl/openuuyc) by djkcyl and
  contributors — the original Rust UU Remote client.
- **omarchyuu** (https://github.com/keatsyh/omarchyuu) — the Linux
  adaptation of OpenUUYC.

The signaling layer (`src/signal/`), WebRTC negotiation and connection core
(`src/rtc/`), the PB control-frame subset (`src/pb/`), the forwarding
protocol and transport (`src/forward/`), the KCP control transport
(`src/kcp/`), the RS-FEC GF(256) math (`src/fec.rs`), and portions of the
peer and session orchestration are copied and adapted from those projects.

These portions remain subject to the upstream **OpenUUYC Copyright,
Provenance and License Notice**, reproduced verbatim in
[NOTICE-OpenUUYC](NOTICE-OpenUUYC). That notice is not an open-source
license: beyond what it and applicable law grant (including
interoperability-related use), rights are reserved by its authors. This
repository is currently **private**; before any public distribution,
review that notice (or obtain authorization from the upstream authors).

## Vendored crates (`vendor/`)

| Crate | Upstream | License |
|---|---|---|
| `webrtc` | webrtc-rs fork with UU-specific patches (omarchyuu) | MIT/Apache-2.0 |
| `kcp` | kcp crate snapshot (omarchyuu) | MIT |
| `dtls`, `interceptor`, `rtcp`, `turn`, `webrtc-ice`, `webrtc-sctp`, `webrtc-srtp`, `webrtc-util` | webrtc-rs sub-crates, patched fork (omarchyuu) | MIT/Apache-2.0 |

These crates carry their own upstream license files inside `vendor/<crate>/`,
which remain effective.

## Other Rust dependencies

All other dependencies are consumed from crates.io under their own
permissive licenses (MIT / Apache-2.0 / BSD / MPL — see each crate).
Ratios and attribution: ratatui, ratatui-kit, tokio, reqwest (rustls/ring),
qrcode, hmac/sha2, uuid, clap, prost.

## Protocol and marks

UU Remote is a NetEase product. Protocol fields and behavior were studied
through reverse engineering for interoperability; uunet is not affiliated
with, endorsed by, or certified by NetEase, and grants no right to use
NetEase services. The official service terms apply:
https://adl.netease.com/d/g/uuremote/c/licenseandservice?type=pc&direct=1
