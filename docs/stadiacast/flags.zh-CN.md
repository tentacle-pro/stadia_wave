# stadiacast 参数速查（场景 → 命令）

> 面向 stadiacast 1.3.0。命令里的 `<router>` 是上游 DNS（通常是路由器），`<mac>` 是接收端所在
> 电脑在手柄网络里的地址，`<controller>` 是手柄的 IP。参数语义以 `stadiacast --help` 与原文文档为准，
> 本文是按场景把常用组合拼好，便于对照。

## 0. 三个前置条件（不满足时加什么参数都没用）

1. **DNS 必须是这台电脑。** 手柄解析 Google 域名时要问到接收端（路由器 DHCP 的 DNS 选项、或热点下发）。
2. **接收端要能主动连到手柄。** 不只是手柄能连进来：STUN/DTLS 是接收端往手柄的 UDP 端口发起的。
3. **本机到局域网的出站不能被拦。** macOS 上「系统设置 → 隐私与安全性 → 本地网络」必须允许
   运行它的程序（Zed / Terminal / Wave Companion）；改动后要**重启那个程序**才生效。公司/访客网络里
   AP 隔离（client isolation）也会拦掉这一步。

第 2、3 条不满足时的典型症状是：HTTP 全部 200、手柄也“注册成功”，但媒体面永远起不来。

## 1. 最小可用（只要输入、震动、控制接口）

```sh
stadiacast --preset live \
  --dns-upstream <router> \
  --advertise-ip <mac> --controller-ip <controller>
```

`--preset live` 已包含：SNTP（UDP 123 应答 `time.google.com`）、`--clearcut-block tls`、
`--bind all`、443/80/8008、UDP 40000/40001。**它不包含 `--start-game`，也不包含任何虚拟手柄。**

## 2. 参数分组

| 分组 | 参数 | 作用 |
| --- | --- | --- |
| 网络身份 | `--advertise-ip` / `--controller-ip` | 写进 ICE 候选和 DNS 应答的地址；同时决定“走哪块网卡”（多网卡/多网段必填） |
| 监听 | `--bind` / `--https-port` / `--http-ports` / `--udp-port` / `--game-udp-port` | 改端口或只监听某块网卡；`--http-ports ''` 关掉明文端口 |
| DNS 助手 | `--dns-upstream` / `--dns-redirect` | 起 DNS 接收器并指定上游；`--dns-redirect` 追加要指回本机的域名 |
| 时间 | `--sntp` / `--no-sntp` / `--clock-pin` | 应答 `time.google.com`；`--clock-pin auto` 用证书有效期中点 |
| 日志/分析 | `-v` / `--log-dir` | `-v` 在控制台显示冗长字段；`--log-dir` 改运行目录（默认 `captures/<时间戳>`） |
| 音频 | `--audio-source` / `--audio-bitrate` / `--audio-application` / `--audio-volume` | 送往手柄耳机口 |
| 麦克风 | `--mic` / `--mic-device` / `--mic-record` / `--mic-pcm` | 手柄麦克风变成设备或录成文件 |
| 游戏会话 | `--start-game [KEY]` / `--start-game-delay` / `--rumble-refresh` / `--rumble-dither` | **震动与游戏输入的通道** |
| 虚拟手柄 | `--gamepad vigem\|ojd\|uinput\|uhid` / `--gamepad-profile` / `--gamepad-deadzone` | 把输入变成系统手柄 |
| 隐私/离线 | `--clearcut-block {off,tls,dns}` / `--stadiaconfig` | 是否让分析上报出网、如何应答配置请求 |

## 3. 场景 → 命令

### 3.1 只验证链路能不能通（不装任何驱动）

```sh
stadiacast --preset live --dns-upstream <router> \
  --advertise-ip <mac> --controller-ip <controller> -v
```

看到 `display_state=ready` 和 `[input] press=['a'] ...` 就算通。

### 3.2 要震动

```sh
stadiacast --preset live --dns-upstream <router> \
  --advertise-ip <mac> --controller-ip <controller> --start-game
```

`--start-game` 在 READY 后发 `StartGame`，游戏会话是通往马达的唯一通路；之后可以

```sh
curl -d '{"left":40000,"right":40000,"ms":1500}' http://<mac>:8008/_rumble
```

不带它时**输入照常，但 `rumble_sent` 永远不会出现**。`--gamepad` 会自动打开 `--start-game`
（日志里那条 `gamepad_start_game`）。

### 3.3 macOS 虚拟手柄（Xbox 手柄身份）

```sh
stadiacast --preset live --dns-upstream <router> \
  --advertise-ip <mac> --controller-ip <controller> --gamepad ojd
```

- 需要 OpenJoystickDriver 0.6.0-alpha.1 正在运行，并在“辅助功能”（必要时“输入监控”）里获批。
- 驱动扩展状态是 `[activated waiting for user]` 时，游戏手柄建不起来，日志是
  `gamepad_failed ... E2005: The service did not reply within 0.5s`。
- 游戏不认默认值时换 `--gamepad-profile sdl-compatible` 或 `generic`；要原始摇杆值加
  `--gamepad-deadzone off`。

### 3.4 Windows / Linux 虚拟手柄

```sh
stadiacast --preset live --dns-upstream <router> --gamepad vigem     # Windows，需 ViGEmBus
stadiacast --preset live --dns-upstream <router> --gamepad uinput    # Linux，需 /dev/uinput 权限
stadiacast --preset live --dns-upstream <router> --gamepad uhid --gamepad-deadzone off  # Linux，真 Stadia 身份
```

### 3.5 耳机音频

```sh
stadiacast --preset live --dns-upstream <router> --audio-source tone        # 先证明通路
stadiacast --preset live --dns-upstream <router> --audio-source system     # 串流电脑声音
stadiacast --preset live --dns-upstream <router> --audio-source device:NAME --audio-devices
```

macOS 需要虚拟输出（BlackHole 2ch）；音量用 `--audio-volume`，媒体音量键对它无效。

### 3.6 手柄麦克风

```sh
--mic ptt --mic-device      # 按住 Assistant 才开，作为电脑麦克风
--mic on  --mic-device      # 连着手柄就一直开
--mic ptt --mic-record      # 每次按住录一个 Ogg Opus；加 --mic-pcm 再出一份原始 PCM
```

`--mic` 只决定“什么时候开”，`--mic-device` / `--mic-record` 决定“声音去哪”，可同时给。
macOS 用 BlackHole 16ch，Windows 用 VB-CABLE，Linux 由 pactl/pacat 创建。

### 3.7 多手柄

无需额外参数：日志和 `/_status` 里是 `c0`、`c1`……，控制请求用 `?controller=c1` 或 JSON 里的
`"controller"` 字段指定目标。

### 3.8 首次配网（手柄从没连过 Wi-Fi，或被复位过）

```sh
stadiacast-provision scan
stadiacast-provision provision --address <ADDR> --ssid <SSID>
```

手柄状态灯橙色闪 = 在等凭据。已经配过网的手柄要重新进设置模式：关机后按住 **Y** 再按 Stadia 键。

### 3.9 完全离线（不让手柄碰真实 Google）

```sh
stadiacast --preset live --dns-upstream <router> \
  --dns-redirect connectivitycheck.gstatic.com,clients3.google.com,time.google.com \
  --stadiaconfig file:<path>
```

手柄开机时除了 `*.googleapis.com` 还会碰两个域名，默认都被**转发**给真实 Google：

- `connectivitycheck.gstatic.com`（`/generate_204`）：连通性检查。指回接收端后由它直接应答 204
  （日志里 handler 记为 `netcheck`）。
- `clients3.google.com`：取手柄配置（`stadiaconfig`）。指回接收端后，默认的 `passthrough` 会由**这台
  Mac** 代取（仍然出网），要彻底离线就配 `--stadiaconfig file:<path>` 或 `json:{...}`。

注意 `--dns-redirect` 是**整份替换**附加域名列表（`googleapis.com` 系列之外），默认只有
`time.google.com`，追加时别忘了把它写回去。另外别用 `--clearcut-block dns`：固件拿到 NXDOMAIN 后会
回落到 `8.8.8.8` 直接连真机，`tls` 才是阻断分析上报的正确做法。

### 3.10 需要更多日志

```sh
stadiacast ... -v --log-dir /tmp/sc-run
```

每次运行都会写 `server.jsonl`（含 `event=input`）、`dns.jsonl`、`sntp.jsonl`，与 `-v` 无关，
不依赖控制台也能复盘。

## 4. 症状对照表

| 日志/现象 | 含义 | 处理 |
| --- | --- | --- |
| `rtc_socket_error ... EHOSTUNREACH`，`stun_in=0` | 本机到局域网的出站被拦（权限/防火墙/AP 隔离） | 授予「本地网络」权限并重启对应程序；关掉 AP 隔离 |
| `tls_client_hello` + `tls_refused play.googleapis.com` | `--clearcut-block tls` 主动拒绝分析上报 | 正常，不是故障 |
| `clients3.google.com` / `connectivitycheck.gstatic.com` 被 `forward` 到真实 IP | 默认行为：配置请求与连通性检查 | 要离线就 `--dns-redirect` + `--stadiaconfig file:` |
| 注册 200、`display_state=ready`，但无 `[input]` | 只是没人按键（输入按变化上报） | 按几下 A / L1 / R1 再看 |
| 有 `[input]`，无 `rumble_sent` | 没开游戏会话 | 加 `--start-game`（或 `--gamepad`） |
| `gamepad_failed ... E2005` | OJD 服务没响应 / 驱动扩展没批准 | 打开 OJD，批准扩展，再重启接收端 |
| `gamepad_failed ... ViGEmBus` 之类 | 没装虚拟手柄驱动 | 装 ViGEmBus（Windows） |
| `helper_exited` 提到 53 / 123 | 端口被别的程序占用 | 关掉占用者，或 `--no-sntp`；53 必须可用 |
| `port_busy` | 80/8008 被占 | 改 `--http-ports` 或关掉占用者 |
| 首次配网后灯一直白闪、按键只震动 | 第一次启动本来就不会连上 | 等它注册，关机再开即可 autolink |
| 手柄电量低/不连 | 与 stadiacast 无关 | 充电；注意刷机工具要求 ≥10% |

## 5. 端口清单

| 端口 | 协议 | 用途 |
| --- | --- | --- |
| 53 | UDP/TCP | DNS 接收器 |
| 123 | UDP | SNTP（`time.google.com`） |
| 443 | TCP | 手柄唯一会连的 HTTPS 端口 |
| 80、8008 | TCP | 控制接口（`/_status`、`/_rumble`、`/_mic`、`/_audio`） |
| 40000、40001 | UDP | 第一个手柄的显示链路与游戏会话；每多一个手柄 +2 |

## 6. Wave Companion（本仓库）实际使用的参数

`apps/bridge/src/stadiacast.rs` 的 `launch()`：

```sh
--preset live --dns-upstream <ip> --controller-ip <ip> --advertise-ip <ip> \
--mic off --mic-record --start-game --log-dir <run_dir>
```

即：**不启用虚拟手柄**，麦克风常关但录音，**开启了游戏会话**（所以震动可用）。
`run_dir` 为 `~/Library/Application Support/dev.stadiawave.bridge/captures/<uuid>/`，
里面同时有 `stdout.log`、`stderr.log` 和 stadiacast 自己的 `server.jsonl`，事后可直接复盘。
