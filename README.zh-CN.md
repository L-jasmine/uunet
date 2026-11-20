# uunet

[English](README.md)

一个基于 UU 远程协议的终端 UI(TUI)工具:用你自己的 UU 账号登录,查看名下设备,在你自己的多台机器之间建立**双向 TCP 端口映射**——不需要桌面客户端。

> ⚠️ **免责声明** —— uunet 是基于逆向工程协议知识的非官方第三方客户端,与网易无隶属、认可或认证关系。你必须使用自己的账号,并且只连接你有权控制的设备。使用网易服务须遵守官方服务条款:
> https://adl.netease.com/d/g/uuremote/c/licenseandservice?type=pc&direct=1
>
> 📮 **侵权联系删除** —— 如本仓库任何内容侵犯了你的权利,请到
> https://github.com/L-jasmine/uunet/issues 提 issue(或联系仓库所有者),
> 经核实后会立即删除相关内容。
>
> 📢 **对官方产品的期望** —— 强烈希望网易为 **Android 客户端加入端口映射功能**,并推出**官方 Linux 客户端**。

## 功能

- **登录** —— 手机端扫码,或从本地 token 文件恢复会话(未过期免扫码)
- **设备列表** —— 名下机器与在线状态
- **主控方向** —— 把*其他节点*的 TCP 端口映射到本机:加入目标房间、协商 WebRTC、通过认证数据通道转发本地连接
- **被控方向** —— 把*本机*端口暴露给*其他节点*:在线登记、接受端口映射会话、按你配置的拨号白名单逐连接外呼
- **受限环境韧性** —— netlink/getifaddrs 被沙箱或安全策略禁用时,ICE 回退到默认路由接口,STUN/中继路径照常可用

## 构建

Linux x86_64(以 Arch 系发行版为主):

```bash
git clone https://github.com/L-jasmine/uunet.git
cd uunet
cargo build --release
```

除 Rust 工具链外无需额外系统包;WebRTC/KCP 协议栈在 `vendor/` 目录内,通过 `[patch.crates-io]` 接入。

## 使用

```bash
./target/release/uunet          # 启动 TUI
./target/release/uunet --host   # 登录后同时启用被控(端口映射)服务
```

各页面底部有键位说明:

- **登录** —— 手机端扫码
- **设备** —— `Enter` 进入选中设备的映射页,`h` 进入被控白名单页
- **映射页** —— `a` 加规则、`空格` 启停、`c` 连接/断开会话、`d` 删除
- **被控页** —— 管理拨号白名单(本机允许被要求连接的目标)

### 配置文件

| 路径 | 用途 |
|---|---|
| `~/.config/uunet/identity.json` | 本机设备身份(client_id、服务端签发的 device_id) |
| `~/.config/uunet/session.json` | 登录 token(chmod 600,扫码成功后写入) |
| `~/.config/uunet/host.json` | 被控侧拨号白名单 |
| `~/.config/uunet/rules/<device>.json` | 按设备的端口映射规则 |
| `~/.uunet/logs/` | 滚动日志 |

## 许可与致谢

uunet 是 [OpenUUYC](https://github.com/djkcyl/openuuyc) /
[omarchyuu](https://github.com/keatsyh/omarchyuu) 实现的移植与改编,本仓库中信令、WebRTC、转发协议的相当一部分代码源自这两个项目。

- **移植部分**(信令、RTC 协商、PB 控制帧、转发协议与传输、在线流程)仍受 [OpenUUYC 版权、出处与许可声明](NOTICE-OpenUUYC)约束,该文件以原文完整保留于 `NOTICE-OpenUUYC`。该声明**不是**开源许可:超出其及适用法律所授范围的权利由原作者保留,互操作相关的使用以其描述的边界为准。
- **uunet 自有原创代码**(ratatui 界面层、NRD 客户端粘合、配置与会话持久化、打包脚本)以 [MIT 许可](LICENSE) 发布。

vendor 依赖与出处细节见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

## 致谢

- **[OpenUUYC](https://github.com/djkcyl/openuuyc)** 原作者 **djkcyl** 及贡献者 —— 原始 Rust 版 UU 远程客户端,uunet 移植的协议实现、契约与代码均来自该项目
- **[omarchyuu](https://github.com/keatsyh/omarchyuu)** —— OpenUUYC 的 Linux 适配,uunet 移植的 ICE/SDP/vendor 接线、转发协议扩展及多数模块以其为基础
- **[webrtc-rs](https://github.com/webrtc-rs/webrtc)** 及 `vendor/` 内的补丁 fork —— WebRTC/SCTP/ICE 基础
- **[ratatui](https://github.com/ratatui/ratatui)** 与 **[ratatui-kit](https://github.com/yexiyue/ratatui-kit)** —— 终端 UI 框架
- **网易 UU 远程** 为网易公司产品;名称仅用于标识互操作目标。uunet 与网易无隶属、认可或认证关系,亦不授予任何使用网易服务的权利
