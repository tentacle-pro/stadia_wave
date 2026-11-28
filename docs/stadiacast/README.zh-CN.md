
> 本文是 stadiacast 1.3.0 附带的 `README.md` 的简体中文翻译（非官方译文），原文见
> https://github.com/jkfran/stadiacast 。命令行参数、固件标识与技术名词保留英文。
> 本文只做翻译，不构成对项目的背书；以原文为准。

Google 在 2023 年 1 月关闭了 Stadia，一起关掉的还有 Stadia 手柄 **Wi-Fi 模式** 所依赖的云端接口。
Stadiacast 是一个运行在你自己网络里的小型服务器，用来顶替这些接口，让你手里的手柄继续和你的电脑配合工作：
它加入你的 Wi-Fi、连接、保持在线、上报每一个按键、摇杆和扳机，按请求震动，并通过手柄的耳机口播放声音。

> **状态：** 在运行 Wi-Fi 固件的手柄上可端到端工作（以 `ota.282115` 测试），支持多个手柄同时使用。
> 仍有粗糙之处。问题请发到
> [Discussions](https://github.com/jkfran/stadiacast/discussions)，bug 发到
> [issues](https://github.com/jkfran/stadiacast/issues)；非常欢迎提交 PR。
> 手柄已经被你刷成蓝牙模式？可以刷回来，见[运行要求](#运行要求)。

## 主要特性

- 🛠️ **不修改固件。** 接收端直接讲手柄原生的协议，从不向手柄写入任何东西。
- 🔒 **不需要账号，不依赖云服务。** 输入、震动和耳机音频只留在手柄和你电脑之间。
- 📜 **无需获取任何东西。** 接收端自带一份手柄认可的证书：解压、运行、唤醒手柄即可。
- 🎮 **Windows 上的 PC 手柄。** 每个手柄都会变成游戏眼里的 Xbox 360 手柄，游戏的震动会回到手柄上。
- 🍎 **macOS 上的手柄。** 通过 OpenJoystickDriver 应用，每个手柄都会变成 Mac 游戏眼里的 Xbox 手柄，含震动。
- 🎧 **耳机音频。** 电脑正在播放的声音、或任意音频文件，都能串流到手柄上插的耳机。
- 🎤 **手柄的麦克风。** 可以变成电脑的一个麦克风供语音聊天使用（一直开、或按住 Assistant 键时才开），也可以录成文件。
- 👥 **同时支持多个手柄**，每个手柄各自是一个玩家。

## 快速开始

```sh
pip install 'stadiacast[ble,audio,gamepad]'           # 或者解开 releases 页面上的可执行文件
stadiacast --preset live --dns-upstream 192.168.1.1   # 你的路由器：DNS 接收器把其余查询转发给它
```

1. **网络。** 把手柄和接收端放在同一个网络里，并让接收端所在的机器充当手柄的 DNS 服务器；
   最简单的做法是在这台机器上开热点（[docs/setup.md](setup.zh-CN.md#网络)）。
2. **只在首次开机时需要。** 全新开箱、或被复位过的手柄会停在蓝牙设置模式里等待 Wi-Fi 凭据：

   ```sh
   stadiacast-provision scan
   stadiacast-provision provision --address <ADDR> --ssid <你的SSID>
   ```

3. **启动接收端**，用上面那条命令；需要虚拟手柄时，[Windows](#虚拟手柄windows) 上加
   `--gamepad vigem`，[macOS](#虚拟手柄macos) 上加 `--gamepad ojd`。从源码目录运行时，
   `tools/start_receiver.sh` 会在后台按 `.env` 文件里的配置做同样的事（见 `.env.example`）。
4. **唤醒手柄**：按住 Stadia 键约 1 秒，在第一次震动时松手。两声短震动加常亮表示它找到了接收端；
   如果只是闪烁、没有震动，那是一次很短的“上报唤醒”，再按一次即可。
5. **观察输入。** 控制台每收到一份上报就打印一行；`GET http://<receiver>/_status` 显示会话状态、
   电量以及是否插着耳机；如果是通过 PyPI 或源码安装的，`stadiacast-show-input captures/<stamp>`
   可以把一次运行汇总出来。
6. **震动。** 需要先用 `--start-game` 启动，因为游戏会话是通往马达的唯一通道，然后 POST 0 到 65535 的强度：

   ```sh
   curl -d '{"left":40000,"right":40000,"ms":1500}' http://<receiver>/_rumble   # 脉冲
   curl -d '{"left":65535,"right":20000}' http://<receiver>/_rumble              # 保持
   curl -d '{"left":0,"right":0}' http://<receiver>/_rumble                     # 停止
   ```

7. **更多手柄**各自拥有自己的会话，日志和 `/_status` 里是 `c0`、`c1`……；用 `?controller=c1`
   或 JSON 体里的 `"controller"` 字段指定控制请求的目标。
8. **停止**用 Ctrl-C；如果是启动脚本拉起的接收端，用 `tools/stop_receiver.sh`。

`--preset live` 是一组经过端到端验证的参数；显式给出的参数会覆盖它，`stadiacast --help` 列出了所有可调项。

## 工作原理

1. **DNS。** 手柄必须把接收端所在的机器当作自己的 DNS 服务器。接收端会拉起一个小型 DNS 服务器，
   把它需要的那些 Google 域名指向自己，其余全部转发给上游。
2. **HTTPS。** 固件用内置的 Google Trust Services 根证书校验服务器证书，但不校验主机名、不校验有效期、
   也不检查吊销状态，所以一条 GTS 证书链（接收端自带的这条）就足以应答所有 Google 域名。
3. **WebRTC。** 手柄在 `DiscoverDisplay` 请求里提出一个 WebRTC 会话；接收端通过 GMsg 推送通道作答，
   终结 STUN 和 DTLS，并扮演 DTLS 之后 RPC 握手中显示端那一侧，直到手柄进入 READY 并开始上报输入。
4. **游戏会话。** 收到 `StartGame` 后，手柄会再开一个用于游戏串流的会话。这个会话同样承载输入，
   而且是通往手柄震动马达的唯一通路。

[docs/protocol.md](https://github.com/jkfran/stadiacast/blob/main/docs/protocol.md)
详细描述了协议，[docs/architecture.md](https://github.com/jkfran/stadiacast/blob/main/docs/architecture.md)
描述代码如何组织。

## 运行要求

- 一台运行 Wi-Fi 固件的 Stadia 手柄（以 `ota.282115` 测试）。刷成蓝牙模式的手柄可以用社区工具刷回来；
  Stadiacast 本身从不写固件。
- 一台位于手柄所在网络中的 Windows、macOS 或 Linux 电脑，充当手柄的 DNS 服务器。
  参考装置是 macOS 的“互联网共享”热点。在 Linux 上，绑定 53、80、123、443 端口需要 root 或
  `CAP_NET_BIND_SERVICE`；在 Windows 上可能需要放行防火墙让手柄进来
  （[docs/setup.md](setup.zh-CN.md#网络)）。
- 基础功能不需要别的东西：接收端自带手柄认可的证书（[换成你自己的](setup.zh-CN.md#证书)是可选项），
  可执行文件里已经包含 Python。从 PyPI 或源码安装则需要 Python 3.11 或更新版本。
- 可选项：ffmpeg，用于[耳机音频](#耳机音频)和[手柄麦克风](#手柄麦克风)；Windows 和 macOS 上做麦克风
  需要一个虚拟音频线；[Windows](#虚拟手柄windows) 和 [macOS](#虚拟手柄macos) 上的虚拟手柄需要驱动
  （[Linux](#虚拟手柄linux) 上的不需要）；`tcpdump` 用于抓包。

## 安装

**可执行文件。** 每个 [release](https://github.com/jkfran/stadiacast/releases) 都包含 Windows x86_64、
macOS（Apple 芯片）和 Linux x86_64 的压缩包，里面是 `stadiacast` 和 `stadiacast-provision` 两个
单文件程序，自带 Python。解压后在终端里运行 `stadiacast` 即可。这些构建没有签名：macOS 会在程序第一次
启动时拦截它，需要在“系统设置 → 隐私与安全性 → 仍要打开”里放行，或者用
`xattr -d com.apple.quarantine stadiacast stadiacast-provision` 清掉两个程序的隔离标记；
Windows SmartScreen 可能会警告。

**从 PyPI 安装**（需要 Python 3.11 或更新）：`pip install 'stadiacast[ble,audio,gamepad]'`
（或 `uv tool install 'stadiacast[ble,audio,gamepad]'`）；这些 extras 都是可选的，见下文。

**从源码安装**，用 [uv](https://docs.astral.sh/uv/)：

```sh
git clone https://github.com/jkfran/stadiacast.git
cd stadiacast
uv sync --all-extras
```

或者用 pip：`pip install -e ".[ble,passthrough,audio,gamepad]"`。从 PyPI 或源码安装会多出第三个命令
`stadiacast-show-input`。`ble` extra 是蓝牙配网；`audio` 是[耳机音频](#耳机音频)采集，以及在
Windows 和 macOS 上把[手柄麦克风](#手柄麦克风)做成设备；`gamepad` 是[虚拟手柄](#虚拟手柄windows)；
`passthrough` 让接收端替手柄去 Google 取它的配置（`stadiaconfig`），这只在把 `clients3.google.com`
重定向到接收端时才会发生（默认不重定向）。`tools/start_receiver.sh` 会自己找到虚拟环境。

## 输入

进入 READY 后，手柄会在显示链路上上报每一次变化。接收端把它解码成事件：具名按键、摇杆轴
（-32768 到 32767，Y 轴向上）和扳机（0 到 255），按键位采用 XInput 布局。完整的上报格式见
[docs/protocol.md](https://github.com/jkfran/stadiacast/blob/main/docs/protocol.md#input-report-controllertodisplay-3)。

## 耳机音频

接收端可以把声音播放到手柄上插的耳机。任何音源都用带 libopus 的 ffmpeg 编码，所以 ffmpeg 必须在
`PATH` 里：

```sh
stadiacast --audio-source tone          # 440 Hz，不需要任何音频环境就能验证通路
stadiacast --audio-source file:x.ogg    # 任何 ffmpeg 能解码的文件
stadiacast --audio-source system        # 这台电脑正在播放的声音
stadiacast --audio-source device:NAME   # 某个采集设备；--audio-devices 会列出它们
```

显示链路进入 READY 后音频流才开始。`--audio-bitrate` 设置 Opus 码率（默认 64 kbit/s），
`--audio-application` 选择偏向低延迟（`lowdelay`，默认）还是音乐（`audio`），`--audio-volume`
设置增益。`/_status` 在 `playback` 下报告音频流，`/_audio` 可以在接收端运行期间启动、停止和调整它
（[docs/setup.md](setup.zh-CN.md#运行)）。

`system` 和 `device:NAME` 是采集声音。在 macOS 和 Windows 上这需要 `audio` extra，可执行文件与
`uv sync --all-extras` 都包含它。在 macOS 上，把声音送到一个虚拟输出设备（例如 BlackHole，
`brew install blackhole-2ch`），单独使用，或与扬声器一起组成“多输出设备”；音量键对它不起作用，
所以要在应用里或用 `--audio-volume` 设置音量。Windows 上采集的是默认输出的环回，音量滑块和静音会
作用到耳机（`--audio-follow-volume off` 发送原始混音）。Linux 上 `system` 通过 PulseAudio 或
PipeWire 的 `pactl` 和 `parec`（`pulseaudio-utils` 包）录制默认输出的 monitor，接收端以 `sudo`
运行时也一样；音量要自己用 `--audio-volume` 设，因为音量键不作用于 monitor。Linux 上的
`device:NAME` 是一个 PortAudio 输入，需要 `audio` extra 和 PortAudio 库
（`apt install libportaudio2`）。音频流如何搭在显示链路上，见
[docs/protocol.md](https://github.com/jkfran/stadiacast/blob/main/docs/protocol.md#audio-to-the-headset-jack)。

## 手柄麦克风

接收端可以把某个手柄的麦克风变成这台电脑的麦克风，供语音聊天或任何录音程序使用，也可以把它录成文件。
作为设备使用时，声音由 ffmpeg 解码，所以 ffmpeg 必须在 `PATH` 里：

```sh
stadiacast --mic ptt --mic-device       # 按住 Assistant 键时才是麦克风
stadiacast --mic on --mic-device        # 手柄连着就一直开
stadiacast --mic ptt --mic-record       # 改为录进本次运行的目录
```

`--mic` 决定麦克风何时打开，`--mic-device` 和 `--mic-record` 决定声音去哪里，两者可以同时给。
两次按键之间设备是静音的，就像一个没人对着说话的麦克风。多个手柄共用一个设备：它们的声音会叠加。
接收端启动时会告诉你，在监听程序里该选哪个麦克风：

- **Linux：** “Stadia Controller microphone”。接收端通过 `pactl` 和 `pacat`
  （`pulseaudio-utils` 包）在 PulseAudio 或 PipeWire 里创建它，停止时移除，以 `sudo` 运行时也一样。
  它旁边出现的输出“Stadia Controller microphone feed”是声音进入设备的通路：不要往里播放别的东西。
  接收端停止时，声音服务器会把原来监听这个麦克风的程序切到别的设备上，就像拔掉麦克风时那样。
- **Windows：** `CABLE Output`。Windows 上程序无法创建麦克风，所以接收端播进一条虚拟音频线：
  安装 [VB-CABLE](https://vb-audio.com/Cable/)（以管理员身份运行安装程序，然后重启）。如果安装过程
  把 `CABLE Input` 设成了 Windows 的默认输出，请回到声音设置里重新选择你的扬声器。
- **macOS：** `BlackHole 16ch`，一个充当同样角色的环回设备（`brew install blackhole-16ch`，然后重启）。
  [耳机音频](#耳机音频)仍然用 BlackHole 2ch。没有 16ch 时，麦克风会退而使用另一个环回设备，当耳机的
  声音不是从 BlackHole 2ch 采集时，2ch 也会被用上。不要往麦克风对应的设备里送别的声音：作为 Mac
  声音输出的环回设备（单独使用或位于多输出设备中）里会有 Mac 自己的声音。

在 Windows 和 macOS 上，`--mic-device NAME` 把声音播进另一个设备（用名字的一部分指定），该设备需要
`audio` extra，可执行文件与 `uv sync --all-extras` 都包含它。

`--mic-record` 写 Ogg Opus 文件，第一个手柄在 18:49:30 的录音是 `mic_c0_184930.ogg`，播放器可以在
文件还在写的时候打开它。用 `ptt` 时，每一次按住都是一个独立的文件。`--mic-pcm` 还会在每段录音到达时
把它解码成同名 `.pcm` 文件：有符号 16 位小端样本、48 kHz、单声道，每秒 96 kB。没有任何东西会删除录音。

麦克风开启时手柄的灯会闪。手柄上插着耳机时，发送的是耳机麦克风的声音。麦克风需要一点时间启动：
用 `ptt` 时，等灯开始闪再说话，比这更短的按压不会带上任何声音。

`/_status` 在 `microphone` 下报告麦克风、在 `mic_device` 下报告设备，`/_mic` 同时显示两者，并可在
接收端运行期间切换麦克风（[docs/setup.md](setup.zh-CN.md#运行)）。手柄如何发送声音见
[docs/protocol.md](https://github.com/jkfran/stadiacast/blob/main/docs/protocol.md#the-controllers-microphone)。

## 虚拟手柄（Windows）

`--gamepad vigem` 把每个手柄变成接收端这台 PC 上游戏眼里的普通 Xbox 360 手柄：

```sh
stadiacast --gamepad vigem              # 每个手柄一个虚拟手柄
```

只有驱动才能创建 Windows 及其游戏认作手柄的设备，所以这需要一次性、带管理员提示的额外安装：
[ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) 驱动，DS4Windows、Sunshine 以及同类工具
用的也是它。作者在 2023 年归档了该项目，1.22.0 是最终版本；它在当前的 Windows 10 和 11 上仍能安装
并工作。必须是桌面版 Windows，因为 Windows Server 缺少这个虚拟手柄所绑定的 Xbox 360 手柄驱动。
没有驱动时，接收端会在启动阶段停下来，并打印一行说明；从 PyPI 或源码安装 `gamepad` extra 时，
会提供运行驱动安装程序的选项。

每份上报都以 XInput 字段送进虚拟手柄，摇杆使用标准死区（`--gamepad-deadzone off` 得到原始摇杆值）；
Stadia 键映射为 guide 键，Assistant 和 Capture 在 XInput 里没有对应项。游戏的震动会通过游戏会话回到
手柄，`--gamepad` 正是为此启动游戏会话。两个手柄分别是玩家 1 和玩家 2，`/_status` 在 `gamepads` 下
列出这些虚拟手柄。

## 虚拟手柄（macOS）

`--gamepad ojd` 通过 [OpenJoystickDriver](https://github.com/xsyetopz/OpenJoystickDriver)（一个在
macOS 12 及以上发布虚拟手柄的应用）把每个手柄变成 Mac 游戏眼里的 Xbox 手柄：

```sh
stadiacast --preset live --dns-upstream 192.168.1.1 --gamepad ojd
```

需要一次性额外安装：把
[OpenJoystickDriver 0.6.0-alpha.1](https://github.com/xsyetopz/OpenJoystickDriver/releases/tag/0.6.0-alpha.1)
放进“应用程序”，打开它，并在“系统设置 → 隐私与安全性 → 辅助功能”里允许它；如果 macOS 把它列在
“输入监控”里，也在那里允许。这个输出就是针对该版本编写的，它也是 MIT 许可下的最后一个版本；
之后的版本仅限非商业使用。该驱动是 alpha 软件，这个输出也很新，所以会有粗糙之处。

应用必须处于运行状态。接收端会在“应用程序”里找它，或在 `PATH` 上找它的 `ojd` 命令，或用
`--ojd-path` 指定的可执行文件。起不来或中途停止的虚拟手柄，会在 `/_status` 的 `gamepads` 下显示
驱动自己的报错，并在日志里留下一条 `gamepad_failed` 记录；手柄下次开机时会重试。

`--gamepad-profile` 决定虚拟手柄对外声称自己是什么：

| profile | 虚拟手柄 |
|---|---|
| `game-controller-compatible`（默认） | 一个 Xbox Wireless Controller。Apple 的 GameController 框架、Chrome 和 SDL2 都能接收它的输入，Chrome 和 SDL2 还会把震动送回来 |
| `sdl-compatible` | 一个 Xbox Adaptive Controller |
| `generic` | 一个普通的 HID 手柄，没有震动，用于自己做了按键绑定的程序 |

当某个游戏不接受默认值时，试试另外两个。

按键、摇杆和扳机的映射与 Windows 上相同，死区也相同（`--gamepad-deadzone off` 得到原始摇杆值）。
Stadia 键映射为 guide 键，macOS 对它的反应在“系统设置 → 游戏控制器”里设置。游戏的震动通过游戏会话
回到手柄，`--gamepad` 正是为此启动游戏会话。每个手柄都是独立的虚拟手柄，最多到驱动允许的四个，
`/_status` 在 `gamepads` 下列出它们。

## 虚拟手柄（Linux）

`--gamepad uinput` 通过内核的 uinput 设备，把每个手柄变成 Linux PC 上游戏眼里的有线 Xbox 360 手柄。
不需要安装任何东西：

```sh
stadiacast --preset live --dns-upstream 192.168.1.1 --gamepad uinput
```

这个虚拟手柄拥有内核给真实手柄的按键、轴和名字，Steam 和大多数 Linux 游戏读手柄所依赖的 SDL 会把它
当作 Xbox 360 手柄。它随手柄的第一份输入出现，并在手柄关机时像无线手柄一样被拔出。

创建虚拟手柄意味着打开 `/dev/uinput`，在多数系统上只有 root 能做。以 root 运行的接收端可以。
不是 root 的话，一条 udev 规则可以把设备交给坐在电脑前的那个人（Steam 自带的规则在安装处就是这么做的）：

```sh
echo 'KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"' |
    sudo tee /etc/udev/rules.d/60-stadiacast.rules
sudo udevadm control --reload-rules && sudo udevadm trigger --name-match=uinput
```

在没人登录的电脑上（例如服务器），把 `TAG` 换成 `MODE="0660", GROUP="input"`，并让接收端以该组成员
身份运行。没有打开设备的权限时，接收端会在启动阶段停下来并打印一行说明。

按键、摇杆和扳机的映射与 Windows 上相同，死区也相同（`--gamepad-deadzone off` 得到原始摇杆值）；
Stadia 键映射为 guide 键。游戏的震动以力反馈形式到达虚拟手柄，再通过游戏会话回到手柄，`--gamepad`
正是为此启动游戏会话。每个手柄都是独立的虚拟手柄，`/_status` 在 `gamepads` 下连同其事件设备一起列出。

### 虚拟 Stadia 手柄（Linux）

`--gamepad uhid` 则通过内核的 uhid 设备，把每个手柄变成有线 Stadia 手柄：一个 HID 设备，带有该手柄的
USB ID（`18d1:9400`）、名字和上报格式，程序会把它当成插着线的手柄。Steam 会列出一个带 **Assistant**
和 **Capture** 键的 Stadia 手柄（Xbox 手柄没有这两个位置），基于 SDL 3 的程序也一样。SDL 2 游戏、
浏览器以及其他程序会用它们为该手柄保留的映射来读它，在那里这两个键至多只是备用键。

```sh
stadiacast --preset live --dns-upstream 192.168.1.1 --gamepad uhid --gamepad-deadzone off
```

Steam 会应用自己的死区，所以这里用 `--gamepad-deadzone off`。两个设备文件需要多数系统只给 root 的权限：
接收端用来创建虚拟手柄的 `/dev/uhid`，以及 Steam 和 SDL 3 读取该手柄所用的 `hidraw` 节点。下面这些规则
把两者都交给坐在电脑前的人（就像 Steam 为它认识的手柄所做的那样）：

```sh
sudo tee /etc/udev/rules.d/60-stadiacast-uhid.rules <<'EOF'
KERNEL=="uhid", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uhid"
KERNEL=="hidraw*", SUBSYSTEM=="hidraw", KERNELS=="0003:18D1:9400.*", TAG+="uaccess"
EOF
sudo udevadm control --reload-rules && sudo udevadm trigger --name-match=uhid
```

第一条规则允许该用户的程序创建任意类型的 HID 设备。在没人登录的电脑上，把它的 `TAG` 换成
`MODE="0660", GROUP="input"`，并让接收端以该组成员身份运行。没有第二条规则时，Steam 和 SDL 3 会像
其他程序一样读取这个虚拟手柄。

游戏的震动两条路都能回到手柄：写入 `hidraw` 节点，或者作为力反馈在事件设备上播放（后者需要内核的
Stadia 驱动 `hid-google-stadiaff`，Linux 6.6 及以后版本提供）。`/_status` 会把每个虚拟手柄的 `hidraw`
节点列为它的 `device`。

## 开发

```sh
uv sync --all-extras      # 接收端、它的 extras 以及开发工具
make check                # ruff、shellcheck、mypy、schema 检查和测试
```

测试不需要硬件：`tests/fake_controller.py` 复现了固件的网络行为。
[CONTRIBUTING.md](https://github.com/jkfran/stadiacast/blob/main/CONTRIBUTING.md)
里有工作流程、如何手动用假手柄对实验模式的接收端做测试，以及如何安全地用真实手柄测试。

## 路线图

1. **首次开机自动连接：** 检测处于 BLE 设置模式的手柄，并自动把它配到网络上（目前
   `stadiacast-provision` 是手动版本）。

## 安全与隐私

接收端在你的局域网里监听，没有任何认证，只适合用于可信网络，例如你自己开的热点。它自己从不联系 Google，
唯一的例外是上面的 `stadiaconfig` 透传。手柄开机时仍会向 Google 发两个请求（连通性检查和它的配置），
DNS 接收器会把它们转发出去（[docs/setup.md](setup.zh-CN.md#网络)）。见
[SECURITY.md](https://github.com/jkfran/stadiacast/blob/main/SECURITY.md)。

## 法律

这是一个由 Stadia 手柄拥有者进行的独立互操作性项目。它与 Google 没有隶属关系，也未被 Google
认可或支持。Stadia 是 Google LLC 的商标。

Stadiacast 不包含 Google 的源代码，也不包含或分发手柄固件：固件始终留在你的手柄上，接收端原样使用
它自带的网络协议。该协议是通过观察手柄并分析其运行的固件恢复出来的，目的仅限于互操作性。有三份
protobuf schema 是依据固件内嵌的描述符复现的；它们属于 Google，[NOTICE](NOTICE) 列出了它们。
请不要在 issue 或 PR 里贴固件镜像、反编译代码或完整的数据包捕获。

Stadiacast 以 [MIT 许可证](LICENSE) 发布，例外情况见 [NOTICE](NOTICE)。
