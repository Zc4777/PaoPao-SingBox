# PaoPao-SingBox

面向 OpenWrt 的 DNS 分流与 Fake-IP 透明代理。LuCI 同时管理 Rust 编写的 PaoPaoDNS-RS 和路由器上的 sing-box。

```text
局域网设备 DNS:53 → paopaodns-rs
                    ├─ 递归上游
                    ├─ 加密上游
                    └─ 自定义 DNS（可选）
```

## 解析行为

- 支持 DNS UDP 和 TCP。上游 UDP 截断时使用 TCP 重试。使用 853 端口的上游通过 DNS over TLS 连接并校验证书，近期使用过的 TLS 连接可复用，阿里 DNS 223.5.5.5 和 223.6.6.6 使用 dns.alidns.com 校验证书。
- 强制转发、强制加密、强制递归按顺序匹配。将原有的三个 `force_*_list.txt` 复制到规则目录即可。规则支持普通域名、`domain:`、`full:`、`keyword:`、`regexp:`。加密上游由 `encrypted` 指定。
- 可加载 CN／PRIVATE MMDB。未标记的 A／AAAA 先查询递归上游，命中 CN 或私有 IP 时保留结果，其他地址改用加密或自定义上游。没有配置 MMDB 时，未标记域名保留可用的递归结果。递归上游出错时可配置国内备用上游，然后尝试加密上游。
- LuCI 的「更新分流数据」按钮会下载 PaoPao-Pref 域名标记。手动部署时，也可以用 Python 工具 tools/prepare-mark-data.py 将 global_mark.dat 转成 global_mark.txt 和 cn_mark.txt，再在 UCI 里指定路径。
- 支持 `no`、`yes`、`only6`、`yes-only6`、`raw` 五档 IPv6 策略。默认按上游屏蔽 SVCB/HTTPS 查询，可用 `allow_service_records` 放行。命令行使用 `yes-only6` 对应原版 `yes_only6`。`recursive_only` 对应关闭智能分流。
- 正向响应按 TTL 存入限量 LRU。符合条件的回答可保存到磁盘，并在重启后恢复。带有 EDNS 选项的查询或响应绕过缓存。递归和 DNSSEC 验证由配置的后端负责。

本版覆盖以上行为。原版的 Redis RDB、`custom_mod.yaml`、`force_ttl_rules.txt`、自动下载数据和 SOCKS5 会话选路属于后续兼容范围。GeoIP 数据和域名标记文件由用户自行准备，遵循各自的数据许可证。

## 本地验证

```sh
cargo test --locked
cargo build --release --locked
target/release/paopaodns-rs --help
target/release/paopaodns-rs --check --rules-dir ./openwrt/paopaodns-rs/files
```

## 在 OpenWrt SDK 构建

在 Linux 上下载与路由器发行版本、目标和子目标一致的 [OpenWrt SDK](https://openwrt.org/docs/guide-developer/toolchain/using_the_sdk)。把项目放到 Linux 文件系统，进入解压后的 SDK 根目录。可用路由器上的 `ubus call system board` 查看型号、版本和 target。

```sh
SOURCE=/absolute/path/to/PaoPaoDNS-RS
./scripts/feeds update packages
./scripts/feeds install rust
mkdir -p package/paopaodns-rs
cp -R "$SOURCE/openwrt/paopaodns-rs/." package/paopaodns-rs/
printf '%s\n' 'CONFIG_PACKAGE_paopaodns-rs=m' >> .config
make defconfig
make package/paopaodns-rs/compile V=s PAOPAODNS_SOURCE_DIR="$SOURCE"
find bin/packages -type f -name 'paopaodns-rs*'
```

改动 Rust 源码后，在重新编译前运行 `make package/paopaodns-rs/clean`。软件包格式由 SDK 决定，OpenWrt 25.12 使用 `.apk`，24.10 使用 `.ipk`。

## 路由器配置

默认随系统启动，将 LAN 的 IPv4/IPv6 TCP/UDP 53 直接交给 PaoPaoDNS-RS。停止服务时撤下劫持。监听端口由服务固定，只有 LAN 和本机可访问。

普通和加密分支默认使用本机 dnsmasq。使用 Unbound 或加密 DNS 客户端时，将对应上游改成该服务的本地监听地址和端口。避免将 dnsmasq 的上游设为 PaoPaoDNS-RS，同时让 PaoPaoDNS-RS 再查询 dnsmasq。

## LuCI

安装新版后端和 `luci-app-paopaodns-rs` 后，进入 **服务 → PaoPaoDNS-RS**。

- 「启动」「停止」「重启」控制当前运行状态，「开机自动启动」控制以后开机时是否启动。
- 进程内存上限使用 Linux 的 `RLIMIT_AS`，单位 MiB。填 0 表示不限。保存并应用后，运行中的进程会以新的上限重新启动。页面同时显示实测 RSS 和生效的虚拟内存上限。
- 规则编辑器管理默认规则目录下的三个 `force_*_list.txt`，单个文件最大 64 KiB。保存前校验规则，服务正在运行时重新加载。

- 「自定义上游（Fake-IP）」填 sing-box 或 Clash 的 DNS 监听地址和端口。填写后自动转发已判定为非 CN 的 A/AAAA 查询与 force_forward_list.txt 指定的域名。留空关闭自定义转发。Fake-IP 地址由该上游生成。IPv4 与 IPv6 开关控制对该上游的 A 和 AAAA 查询，默认仅 IPv4。
- 「更新分流数据」检查 CN 地址库和 PaoPao-Pref 域名标记，下载并校验变更的数据。更新完成后重新加载分流配置。手动指定的自定义数据路径由用户维护。
- 客户端查询与服务日志各显示最近 500 条，当前页签每 3 秒刷新。服务重启后显示本次运行日志，启动时记录从磁盘加载的缓存数量。
- 缓存文件 /etc/paopaodns-rs/cache.bin 一直启用，默认上限 4 MiB。常规正向结果每分钟及正常退出时保存。重启时按剩余有效期和当前规则逐条恢复缓存。更换递归上游后重新查询。Fake-IP 分流回答留在内存缓存。常用的公网 A／AAAA 答案到期前 10% 可由查询触发一次后台刷新。主递归上游的公网答案可在解析失败时提供 5 分钟以内的过期兜底，回复 TTL 最长 5 秒。
- 「平均 DNS 响应时间」统计本次服务启动以来所有已回复请求，从收到完整请求到交给系统发送。

- 「代理编辑器」使用路由器已有的 `/usr/bin/sing-box`。保存 `/etc/paopaodns-rs/proxy/config.json` 或已存在的本地 JSON 节点、规则文件时先校验，启动失败会恢复上一份配置。主配置从本地文件读取。节点、规则集和面板由 sing-box 按配置加载。本地节点和规则文件可放在 `/etc/paopaodns-rs/proxy/providers/`、`rule-sets/` 文件。
- 集成代理的 DNS 入站为 `127.0.0.1:1020`，PaoPaoDNS-RS 通过「自定义上游」转发至此。「局域网代理」管理不代理的客户端 IP。名单中的客户端保留原 DNS 服务器和路由。其余 LAN 客户端发往 `7.0.0.0/8` 与 `fc00::/18` 的 TCP/UDP 进入透明代理。日志页可查看代理服务和当前核心日志。核心缓存使用同目录的 `cache.db`。

从 SDK 构建页面时，同时更新 `luci` feed、安装其 `luci-base` 定义，再将 `openwrt/luci-app-paopaodns-rs` 复制到 SDK 的 `package/`。将 `CONFIG_PACKAGE_luci-app-paopaodns-rs=m` 加入 `.config`，然后构建 `package/luci-app-paopaodns-rs/compile`。

## 上游与许可证

分流规则格式和策略参考 [PaoPaoDNS](https://github.com/kkkgo/PaoPaoDNS)，本项目按 GPL-3.0-only 发布。DNS 报文由 Hickory Proto 解析。
