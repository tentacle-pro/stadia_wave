> 本文是 stadiacast 官方文档 `docs/setup.md` 的简体中文翻译（非官方译文），原文见 https://github.com/jkfran/stadiacast/blob/main/docs/setup.md 。命令行参数与技术标识保留英文。

# 环境配置

如何将 Stadia 手柄和接收端放在同一个网络上、接收端提供什么证书、如何运行它，以及如何阅读它记录的日志。

## 参考装置

```
 Internet ── router (192.168.1.1) ── Ethernet ── Mac (192.168.2.1 on bridge100) ~~ Wi-Fi hotspot ~~ controller (192.168.2.2)
```

这台 Mac 通过 Wi-Fi 共享其以太网连接（系统设置 → 通用 → 共享 → 互联网共享）。macOS 会在 `bridge100` 上分配 `192.168.2.x` 地址，并以这台 Mac 作为网关和 DNS 服务器。接收端运行在这台 Mac 上，在 53 端口启动 DNS 接收器，在 123 端口启动 SNTP 服务器，并监听 443、80 和 8008（HTTPS 与明文 HTTP），以及用于两个 WebRTC 会话的 UDP 40000 和 40001。

`tools/start_receiver.sh` 默认采用这套参考装置：上游解析器为 `192.168.1.1`，手柄位于 `192.168.2.2`，抓包接口为 `bridge100`（`.env.example` 可修改这些设置）。`stadiacast` 本身监听上述端口，并假定手柄位于 `192.168.2.2`；它会检测自己的地址，也就是通往手柄的那个接口上的地址。它没有默认解析器：只有指定 `--dns-upstream` 才会运行 DNS 接收器，只有指定 `--sntp` 才会运行 SNTP 服务器，而 `--preset live` 会启用 SNTP 服务器。

## 网络

唯一的硬性要求是**手柄通过接收端解析域名**。`--dns-upstream <resolver>` 会启动 DNS 接收器：它用接收端的 IP 回答 `*.googleapis.com`、`googleapis.com` 和 `time.google.com`（返回 A 记录；AAAA 查询得到空答案，以便固件留在 IPv4 上），并把其他所有域名转发给该解析器。`--dns-redirect` 设置除 `googleapis.com` 系列之外还要重定向的域名（逗号分隔，默认 `time.google.com`：添加名称时请把它保留在列表里）。固件使用的另外两个域名——`connectivitycheck.gstatic.com`（其连通性检查，`/generate_204`）和 `clients3.google.com`（其配置，`stadiaconfig`）——会像其他域名一样被转发，因此手柄在启动时仍会向 Google 发出这两个请求。

可选方案，从最简单到最复杂：

- **由接收端所在机器充当热点**（即参考装置）。该机器自动成为手柄的 DNS 服务器。
- **使用你现有的 Wi-Fi。** 让路由器的 DHCP 只向手柄下发接收端的 IP 作为 DNS 服务器（可通过静态租约或按客户端 DNS 选项实现）。不要对整个局域网生效：DNS 接收器对所有设备一视同仁地应答，否则其他设备拿到的 `*.googleapis.com` 和 `time.google.com` 会指向接收端，它们的 Google 服务就会失效。
- **独立 DNS 接收器**，例如运行在另一台主机上，任何安装方式都可以：

  ```sh
  stadiacast --helper dns_sink --upstream <router> --redirect-ip <receiver> --log dns.jsonl \
      --redirect '*.googleapis.com' --redirect googleapis.com --redirect time.google.com
  ```

  如果不带 `--redirect`，它只重定向两个 `googleapis.com` 模式，不包括 `time.google.com`。

如果网络不是 `192.168.2.x`，请用 `--controller-ip` 告诉接收端手柄的地址，或手柄所在网络上的任意地址（默认 `192.168.2.2`）：接收端会从通往该地址的接口取得自己的地址，DNS 和 SNTP 日志也会标出该地址的查询。如果这样选错了接口，可用 `--advertise-ip` 设置 DNS 接收器应答时使用的地址，以及 WebRTC 应答中携带的地址。`tools/start_receiver.sh` 会从 `.env` 读取这两个值（`CONTROLLER_IP`、`ADVERTISE_IP`）。

手柄的 Wi-Fi 只支持 WPA2：请让它接入的网络使用 WPA2-Personal（AES），并关闭受保护管理帧（protected management frames），不要使用 WPA3。

注意事项：当被重定向的域名得到 NXDOMAIN 时，固件会回退到 `8.8.8.8`，并通过 NAT 访问真实主机。因此更推荐使用 `--clearcut-block tls`（让 TLS 握手失败），而不是 `--clearcut-block dns`（参见 [protocol.md](protocol.md#7-clearcut-and-the-power-off)）。

接收端使用的端口：

| 端口 | 协议 | 用途 |
|---|---|---|
| 53 TCP/UDP | DNS | DNS 接收器（`--dns-upstream` 会启动它） |
| 123 UDP | SNTP | 为手柄提供时间（`--sntp`，在 `live` 预设中开启） |
| 443 TCP | HTTPS | DNS 接收器重定向到的 Google 端点 |
| 80, 8008 TCP | HTTP | 本地控制端点 `/_status`、`/_input`、`/_events`、`/_rumble`、`/_start_game`、`/_audio`、`/_mic`、`/_shutdown` |
| 40000 UDP | STUN/DTLS | 第一个手柄（`c0`）的显示链路 |
| 40001 UDP | STUN/DTLS | 第一个手柄的游戏串流会话 |
| 40002, 40003 UDP, ... | STUN/DTLS | 第二个手柄（`c1`）的同一对端口，依此类推：基础端口 + 每个手柄 2 个 |

在 Linux 上，绑定 1024 以下的端口需要 root 权限，或者给解释器二进制文件授予 `CAP_NET_BIND_SERVICE`（对 `.venv/bin/python3` 所链接到的文件执行 `setcap cap_net_bind_service=+ep`）。53 端口常常已被 systemd-resolved 或 dnsmasq 占用（NetworkManager 会为它共享的热点运行一个）：此时 DNS 接收器会退出，它的 `helper_exited` 记录会说明这一点。

在 Windows 上，Windows Time 服务运行期间会占用 UDP 123，必须先停止它，SNTP 服务器才能启动：在管理员 PowerShell 中执行 `Stop-Service w32time`（Windows 会在下次启动时再次启动它）。也可以改用 `--no-sntp`，在没有 SNTP 服务器的情况下运行；固件还会从接收端 HTTP 的 `Date` 头和 `GetCurrentTime` 应答中获取时间（[protocol.md](protocol.md#8-time)）。

在 Windows 上，如果手柄与 PC 不在同一子网（例如通过一条专用 Wi-Fi 网络路由到手柄），Windows Defender 防火墙默认会丢弃入站流量：请放行 UDP 53 和 123、TCP 443，以及 WebRTC 端口所在的 UDP 范围（40000 及以上，每个手柄两个）。如果游戏端口被拦截，手柄会一直亮白灯，几秒后 StartGame 失败。

## 证书

固件会用它内置的 Google Trust Services (GTS) 根证书验证服务器证书链，除此之外不检查任何东西：不检查主机名，不检查有效期，也不检查吊销状态。因此，任何由 GTS 签发的证书链都能应答所有 Google 域名，而自签名证书会被拒绝，并触发 TLS 警报 `unknown_ca`（测试中仍然会使用自签名证书，即 `--self-signed`，因为假手柄不做检查）。

**内置证书。** 在未配置任何证书时，接收端提供一条 GTS 证书链，它签发给本项目专用的一个域名，并在其密钥公布之前以密钥泄露（key compromise）为由被吊销：它对手柄来说刚好够用，在其他任何地方都毫无用处。日志会记录一个 `builtin_credential` 事件，包含其主体（subject）、签发者（issuer）和序列号。

**自带证书。** 传入 `--cert`（PEM 证书链，叶子证书在前）和 `--key`，或者把这一对文件放在 `certs/` 下，命名为 `fullchain.pem` 和 `privkey.pem`；`certs/` 中的这一对会优先于内置证书，而命令行中指定但不存在的文件会报错。Google 的公共 CA 可以通过 ACME 为你能够编辑 DNS 记录的域名免费签发证书。请使用一个专用于接收端的域名，因为该证书会被用于应答 Google 的域名；并且只需签发一次：固件不检查有效期，因此永远不需要续期。

```sh
gcloud services enable publicca.googleapis.com          # once, in any GCP project
gcloud publicca external-account-keys create             # prints keyId + b64MacKey
certbot certonly --manual --preferred-challenges dns \
    --server https://dv.acme-v02.api.pki.goog/directory \
    --email <you@example.org> --agree-tos --no-eff-email \
    --eab-kid=<keyId> --eab-hmac-key=<b64MacKey> \
    -d receiver.example.org --config-dir ./le --work-dir ./le --logs-dir ./le
```

把 `fullchain.pem`（完整证书链：中间证书很重要）和 `privkey.pem`（执行 `chmod 600`）复制到 `certs/`；这两个文件都被 git 忽略。

## 运行

`tools/start_receiver.sh` 会在后台以 `--preset live` 启动接收端，并创建一个全新的运行目录 `captures/<stamp>/`。在 macOS 上，如果 `/dev/bpf*` 可读，它还会抓取整个热点接口（`IFACE`）的数据包：执行 `sudo chmod o+rw /dev/bpf*` 可以让它在下次启动前保持可读。`PCAP=0` 跳过抓包，`PCAP_FILTER` 缩小抓包范围。它从环境变量或仓库根目录的 `.env` 文件读取装置设置（参见 `.env.example`），并把任何额外参数传给接收端，这些参数会覆盖预设：

```sh
tools/start_receiver.sh --start-game        # also start the game session once READY
tools/stop_receiver.sh
```

运行目录包含：

| 文件 | 内容 |
|---|---|
| `server.jsonl` | 事件日志：每行一个 JSON 对象，包含 `ts`、`event` 以及各字段 |
| `server.out` | 日志的控制台回显（每个事件一行紧凑文本） |
| `bodies/` | 手柄发送的每个请求体，以原始字节保存 |
| `mic_<controller>_<time>.ogg` | 使用 `--mic-record` 时，手柄麦克风的录音；使用 `--mic-pcm` 时，每个录音旁还有一个 `.pcm` 文件 |
| `dns.jsonl`, `sntp.jsonl` | 辅助进程的日志 |
| `<IFACE>.pcap` | 数据包抓包（macOS），在启用时生成 |

`GET http://<receiver>/_status` 以 JSON 返回实时状态：对每个手柄给出其会话、电量、耳麦、耳麦流（`playback`）和 `microphone`；使用 `--mic-device` 时，在 `mic_device` 下给出承载麦克风的设备；使用 `--gamepad` 时，在 `gamepads` 下给出每个虚拟手柄，包含其 XInput `slot` 或它最终遇到的 `error`；macOS 手柄没有 slot，会给出其 `kind` 和 `profile`；Linux 手柄给出其 `kind` 和 `device`：即游戏读取的事件设备（`/dev/input/event7`），或者在使用 `--gamepad uhid` 时给出手柄的 `hidraw` 节点（`/dev/hidraw3`）。`-v` 还会把体积较大的日志字段（请求体、解码后的 protobuf）也回显到控制台。

`GET /_input` 是简版，适合用于展示或使用手柄操作的程序：对每个手柄给出其链路状态（`display_state`，链路就绪后为 `ready` 或 `input`）、它的 `ip`、距离上次收到消息的秒数（`last_seen_s`）、`battery_percent`、`headset`、`microphone`（是否被要求使用麦克风）、其虚拟 `gamepad`，以及 `input`，即最新报告：按名称给出的按下的按键（`held`）、摇杆（`lx`、`ly`、`rx`、`ry`，范围 -32768 到 32767，向上为正）和扳机（`l2`、`r2`，0 到 255）。`GET /_events` 以服务器发送事件（`event: input`）发送同一文档：连接后立即发送一次，此后每当其中内容发生变化时发送一次，否则每 2 秒发送一次。跟踪手柄的程序应读取这个流，而不是反复轮询，因为每个请求都会成为日志中的一条记录。最多可以同时打开八个流。

```sh
curl -s http://192.168.2.1/_input | jq '.controllers | map_values(.input.held)'
curl -N http://192.168.2.1/_events
```

`POST /_shutdown` 会像按 Ctrl-C 一样停止接收端，连同其辅助进程、耳麦流和虚拟手柄一起停止。该请求只接受通过回环地址发出，因此要从接收端所在的电脑上发起（`curl -X POST http://127.0.0.1/_shutdown`）；这也是在 Windows 上停止后台运行的接收端的方式，因为在 Windows 上无法向它发送 Ctrl-C。

`/_audio` 在接收端运行期间控制耳麦流：`GET` 显示其状态，`POST` 指定一个动作：`{"do": "start", "source": "system"}`（可选带 `volume` 和 `application`）、`{"do": "stop"}`、`{"do": "volume", "volume": 0.5}` 或 `{"do": "mode", "mode": 2}`。有多个手柄时，用 `controller` 指定目标，与 `/_rumble` 相同。

`/_mic` 对某个手柄的麦克风做同样的事。`GET` 显示开关（`on`）、是否正在录音（`record`）、当前打开的录音（`recording`：其 `file`、`seconds` 和 `packets`，使用 `--mic-pcm` 时还有 `decoder` 以及它最近解码内容的电平）、上一个录音（`last`），以及使用 `--mic-device` 时的设备（`device`：其 `name`、程序选择的 `microphone`、无法访问时的 `error`，并在 `voices` 下给出每个手柄到达的声音）。`POST` 指定一个动作：`{"do": "on"}` 和 `{"do": "off"}` 开关麦克风，`{"do": "ptt", "on": true}` 让 Assistant 按键成为其开关，`{"do": "record", "on": true}` 录音，`{"do": "pcm", "on": true}` 同时把录音解码为 PCM。`POST` 只接受通过回环地址发出，与 `/_shutdown` 相同。在手柄链路进入 READY 之前，以及既没有设备也没有录音会接收声音的情况下，对 `on` 的应答是 409：

```sh
curl -d '{"do":"on"}' http://127.0.0.1/_mic
curl -s http://127.0.0.1/_mic | jq '.device.voices'
```

### 唤醒手柄

按住 Stadia 按键约一秒，在第一次振动时松开。一次完整启动大约需要十秒钟：手柄加入 Wi-Fi 并与接收端通信期间指示灯会闪烁，随后**两次短促振动和常亮指示灯**表示它已匹配成功。唤醒会在完整启动和短暂的*报告唤醒*之间交替（指示灯闪烁、无振动、一次 Clearcut 上传，然后重新休眠）：再按一次即可。按住 Stadia 按键 4 秒会关闭手柄。

### 多个手柄

每个表明自身身份的手柄都会获得自己的会话：每条日志行和 `/_status` 中的标签 `c0`、`c1`、……，自己的一对 UDP 端口（见上表），自己的 DTLS 证书和 GMsg 队列。手柄通过固件发送的 id（`controller-device-id` / `controller-cloud-device-id` 请求头、请求体中的 `device_id`）以及接收端分发给每个手柄、固件在其 GMsg 轮询中回传的显示令牌来区分；地址则作为兜底。重启的手柄会保留其标签。

当已知手柄不止一个时，控制端点需要指定目标：

```sh
curl http://192.168.2.1/_status | jq '.controllers | map_values({device_id, ip, display: .session.display.state})'
curl -d '{"left":40000,"right":40000,"ms":1500}' 'http://192.168.2.1/_rumble?controller=c1'
curl -d '{"left":0,"right":0,"controller":"c1"}' http://192.168.2.1/_rumble
```

`stadiacast-show-input` 会为每个手柄打印一个区段。

### 阅读日志

```sh
stadiacast-show-input captures/<stamp>                       # every button press in order, axis ranges
jq -c 'select(.event=="input")' captures/<stamp>/server.jsonl
jq -c 'select(.event|test("rpc_|display_|game_"))' captures/<stamp>/server.jsonl
```

出问题时值得关注的事件：

| 事件 | 含义 |
|---|---|
| 带 `UNKNOWN_CA` 的 `tls_alert` | 手柄拒绝了证书：证书链并非 GTS 签发 |
| 完全没有 `tcp_accept` | 手柄没有通过接收端解析域名（检查 `dns.jsonl`） |
| `gmsg_unattributed` | 来自一个从未注册到当前接收端进程的手柄的 `/gmsg` 轮询（它会在下次启动时注册） |
| 指向 `/log/batch` 的 `http_request`，随后归于沉寂 | Clearcut 断电；确认 `live` 预设（`clearcut_block=tls`）已启用 |
| `display_rpc_unanswered` | 手柄从未应答 `GetControllerStateRequest`；关于 ResolveDiscovery 触发条件，参见 protocol.md |
| `rpc_out_failed` | 在没有 DTLS 会话的情况下尝试了 RPC |
| `game_start_failed` | 手柄拒绝了 `StartGame`（它不在 BROWSE 状态） |
| `rumble_refresh_failed` | 振动持续期间游戏会话断开了 |
| `audio_start_failed` | `--audio-source` 流无法启动（`spec` 是音源，`error` 说明原因）：`PATH` 中没有带 libopus 的 ffmpeg，或者音源被拒绝 |
| `audio_source_invalid` | `--audio-source` 的写法有误，或者它指定的采集源无法在这台机器上建立（没有回环设备、缺少附加组件） |
| `audio_capture`, `audio_capture_failed`, `audio_capture_ended` | 为 ffmpeg 提供输入的采集源（`system`、`device:`）已启动、无法启动或已停止（`error` 说明原因）；失败后 ffmpeg 会退出（`audio_ffmpeg_exited`），随后两者都会重新启动，直到流被停止 |
| `gamepad_created`, `gamepad_failed` | `--gamepad`：已为某个手柄创建虚拟手柄（在 Windows 上 `slot` 是其 XInput 玩家编号；在 macOS 上驱动程序的输入流已启动；在 Linux 上 `device` 是其事件设备，使用 `--gamepad uhid` 时则是其 `hidraw` 节点），或者无法创建或停止工作（`error`，也会在 `/_status` 的 `gamepads` 下显示）；手柄下次启动时会重试 |
| `gamepad_rumble_failed` | 游戏的振动无法转发（游戏会话未启动） |
| `mic_switch` | 手柄被要求开启或关闭其麦克风（`on`），触发来源可能是 `--mic on`（`reason` 为 `autostart`）、Assistant 按键（`ptt`）或 `/_mic`（`api`）；也可能是接收端在自身停止时要求其停止（`stop`），因为收到了未经请求的声音（`stray`），或者因为已经没有任何地方接收声音（`nowhere`）；如果没有 `ticket`，则什么也没发送：链路未就绪（READY），或者（`nowhere`）既没有设备也没有录音会接收声音 |
| `mic_refused` | 手柄以错误（`code`）应答了该请求 |
| `mic_unanswered` | 手柄在一秒内没有应答，也没有声音传来；带 `again` 时表示又发送了一次，总共发送四次 |
| `mic_recorded` | 一次录音结束：其 `file`、`seconds`，以及在 `concealed` 下给出传输途中丢失的数据包 |
| `mic_recording_failed`, `mic_write_failed` | 运行目录拒绝了录音（`error`）；不再继续录音，并且除非有设备承载麦克风，否则麦克风会被关闭 |
| `mic_device` | `--mic-device`：麦克风声音播放到的设备（`name`）、程序选择的 `microphone`，以及当电脑自身的声音也进入该设备时的 `note` |
| `mic_device_lost`, `mic_device_back` | 设备不再接收声音（`error`：声音服务器消失、线缆被拔出）；大约每五秒会重新打开一次，直到它恢复；第二次 `mic_device_lost` 会说明某次尝试失败的原因 |
| `mic_device_note` | Linux：麦克风的输入已变成电脑的声音输出（`note`），或者不再是 |
| 带 `reason` 的 `mic_voice_unavailable`、`mic_voice_ended` | `--mic-device`：用于为设备解码某个手柄麦克风的 ffmpeg 无法启动，或在该手柄连接期间结束；该手柄的下一个数据包会启动另一个 |
| 带 `reason` 的 `mic_pcm_unavailable`、`mic_pcm_ended` | `--mic-pcm`：ffmpeg 无法启动，或在录音结束前结束 |
| `mic_stray` | 开关关闭时收到声音，记录其第一个数据包以及之后每第 500 个；`/_mic` 会在 `stray` 下统计全部 |
| `mic_stream_restarted` | 某个手柄的声音以比之前更小的序列号继续；录音也随之继续 |
| `controller_hardware` | 就绪（READY）时输出一行，包含手柄对自身的描述：产品、硬件和固件版本、序列号、耳麦 |
| `controller_battery` | 电量计发生变化（`percent`）；它每次下降一个百分点 |
| `controller_audio_config` | 耳麦被插入或拔出（`headset`） |
| `port_busy` | 80 或 8008 端口被其他程序占用；接收端会在没有它的情况下继续运行，443 是手柄唯一需要的端口 |
| `helper_exited` | DNS 接收器或 SNTP 服务器（`helper`）结束；`output` 保存其最后写出的几行，其中会指出被其他程序占用的端口 |

## 通过蓝牙进行 Wi-Fi 配网

从未设置过的手柄会广播一个 Bluetooth LE 设置服务，并等待 Wi-Fi 凭据：其状态灯闪烁橙色。要将已经设置过的手柄重新置于设置模式，请先关闭它（按住 Stadia 按键直到灯熄灭），然后按住 Y 并按下 Stadia 按键。随后 `stadiacast-provision` 会以与 Stadia 应用相同的方式发送凭据：

```sh
stadiacast-provision scan                                 # find the controller (Fast Pair 0xFE2C)
stadiacast-provision provision --address <ADDR> --ssid <SSID>      # prompts for the password
```

该工具读取手柄的 RSA 公钥，并用该公钥加密写入网络名称和密码；随后手柄会自行找到该网络。提供密码时，它假定使用 WPA2-Personal (AES)，留空时则假定为开放网络。手柄带着凭据离开设置模式时会断开蓝牙链路，工具到此结束，输出 `Done`。如果手柄无法加入（`STATE -> 0x4f`）或没有任何加入的迹象，工具会以错误结束，并说明应检查什么。

对于使用其他安全方式的网络，请加上 `--wifi-scan`：手柄会先扫描 Wi-Fi 网络，工具会复制它报告的、用于你的网络的安全设置。有些手柄会在扫描几秒后断开 BLE 链路；此时工具会说明这一点，并列出在不使用扫描的情况下手动指定安全设置所需的选项：`--wpa-auth` 和 `--wpa-cipher` 接受 `--help` 中列出的值。

**首次启动时的预期表现：** 手柄加入 Wi-Fi、闪烁白光、通过接收端解析 Google 域名，并在拿到凭据后一分钟内注册到接收端（日志中的 `controller_new`）。无论手柄之前是否配对过，这次启动都不会连接：指示灯会持续闪烁白光，按键只会让它振动。等过这一分钟，然后关闭手柄（按住 Stadia 按键直到灯熄灭）并再次唤醒它：第二次启动会走正常的自动连接流程并连接成功（两次短促振动、白光常亮）。无需输入任何配对码。如果放任不管，手柄会在注册约六分钟后自行关闭，而下一次唤醒会以同样的方式连接。

在 macOS 上，蓝牙地址是 CoreBluetooth UUID，可能会在不同运行之间变化；`--service-data-match HEX` 可以改为通过 Fast Pair 服务数据的一个片段来查找设备（`scan` 会显示该数据）。密码也可以来自 `STADIA_WIFI_PW` 环境变量。`list-services` 和 `windows` 会转储 GATT 表和设备的 JSON 窗口以供调试；[protocol.md](protocol.md#11-wi-fi-provisioning-over-bluetooth-le-ble_provisionerpy) 描述了该设置服务。
