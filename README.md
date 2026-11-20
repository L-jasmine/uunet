# uunet

[简体中文](README.zh-CN.md)

A terminal UI (TUI) tool for the UU Remote protocol: sign in with your own
UU account, see your devices, and set up bidirectional TCP port mapping
between the machines you own — without the desktop client.

> ⚠️ **Disclaimer** — uunet is an unofficial third-party client built on
> reverse-engineered protocol knowledge. It is not affiliated with, endorsed
> by, or certified by NetEase. You must use your own accounts and only
> connect to devices you are authorized to control. The official service
> terms apply to the use of NetEase services:
> https://adl.netease.com/d/g/uuremote/c/licenseandservice?type=pc&direct=1
>
> 📮 **Takedown / infringement contact** — if any content in this
> repository infringes your rights, open an issue at
> https://github.com/L-jasmine/uunet/issues (or contact the repo owner)
> and the offending content will be removed promptly upon verification.
>
> 📢 **Requests to the official product** — we strongly hope NetEase will
> bring **port-mapping support to the Android client** and ship an
> **official Linux client**.

## Features

- **Login** — scan a QR code with the UU Remote mobile app, or restore a
  saved session from the token file (no re-scan until it expires)
- **Device list** — the machines under your account, online state included
- **Controller** — map TCP ports on *other* nodes to your local machine:
  uunet joins the target's room, negotiates WebRTC, and forwards local
  connections over the authenticated data channel
- **Controlled** — expose this machine's ports to *other* nodes: register
  online, accept port-mapping sessions, and dial out per-connection through
  a dial whitelist you control
- ** resilient in restricted environments** — when netlink/getifaddrs is
  denied by a sandbox or security policy, ICE falls back to a
  default-route interface so sessions can still use STUN/relay paths

## Build

Linux x86_64 (Arch-based distros are the primary target):

```bash
git clone https://github.com/L-jasmine/uunet.git
cd uunet
cargo build --release
```

No extra system packages are needed beyond a working Rust toolchain;
WebRTC/KCP stack and codecs are vendored under `vendor/` with
`[patch.crates-io]` overrides.

## Usage

```bash
./target/release/uunet          # start the TUI
./target/release/uunet --host   # + enable the controlled (mapping) service after login
```

Keys inside the TUI are shown at the bottom of each page:

- **Login** — scan the QR code with the mobile app
- **Devices** — `Enter` opens the selected device's mapping page, `h` the
  controlled whitelist page
- **Mapping page** — `a` add rule, `Space` enable/disable, `c` connect or
  disconnect the mapping session, `d` delete
- **Host page** — manage the dial whitelist (targets this machine may be
  asked to connect to)

### Configuration files

| Path | Purpose |
|---|---|
| `~/.config/uunet/identity.json` | local device identity (`client_id`, server-issued `device_id`) |
| `~/.config/uunet/session.json` | login token (chmod 600, written after QR login) |
| `~/.config/uunet/host.json` | dial whitelist for the controlled side |
| `~/.config/uunet/rules/<device>.json` | per-device port-mapping rules |
| `~/.uunet/logs/` | rolling log files |

## License

uunet is a port and adaptation of the
[OpenUUYC](https://github.com/djkcyl/openuuyc) /
[omarchyuu](https://github.com/keatsyh/omarchyuu) implementation, and a
substantial portion of the signaling, WebRTC, and forwarding code in this
repository is derived from those projects.

- **Ported portions** (signaling, RTC negotiation, PB control frames,
  forwarding protocol and transport, presence flow) remain subject to the
  [OpenUUYC Copyright, Provenance and License Notice](NOTICE-OpenUUYC),
  reproduced verbatim in `NOTICE-OpenUUYC`. That notice is **not** an
  open-source license: rights beyond what it and applicable law grant are
  reserved by their authors, and interoperability-related use is subject to
  the boundaries described there.
- **uunet's own original code** (the ratatui UI layer, the NRD client
  glue, configuration/session persistence, and packaging scripts) is
  released under the [MIT License](LICENSE).

See also [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) for vendored
dependencies and provenance details.

## Acknowledgements

- **[OpenUUYC](https://github.com/djkcyl/openuuyc)** by **djkcyl** and
  contributors — the original Rust UU Remote client. The protocol
  implementation, contracts, and code that uunet ports come from this
  project.
- **[omarchyuu](https://github.com/keatsyh/omarchyuu)** — the Linux
  adaptation of OpenUUYC. The ICE/SDP/vendor wiring, forwarding protocol
  extensions, and many of the ported modules in uunet are based on it.
- **[webrtc-rs](https://github.com/webrtc-rs/webrtc)** and the patched
  fork shipped under `vendor/` — the WebRTC/SCTP/ICE foundation.
- **[ratatui](https://github.com/ratatui/ratatui)** and
  **[ratatui-kit](https://github.com/yexiyue/ratatui-kit)** — the terminal
  UI framework.
- **NetEase UU Remote** is a product of NetEase; the names are referenced
  only to identify the interoperability target. uunet is not affiliated
  with, endorsed by, or certified by NetEase, and does not grant any right
  to use NetEase services.
