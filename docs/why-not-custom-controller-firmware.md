# 为什么 Stadia 手柄无法通过刷自定义固件成为语音 Vibe-Coding 硬件

> 本文沉淀自对 `stadiacast` 项目协议栈、Stadia 控制器硬件架构以及 Google 官方蓝牙固件的调研与分析，用于明确 `stadia_wave` 项目的技术边界：**必须保留手柄原厂 Wi-Fi 固件，将无线接入与协议适配下沉到外部接收器（如 ESP32-S3 Wireless Adapter）**。

---

## 1. 原始设想与核心体验诉求

要让 Stadia 手柄成为一支面向技术小白、随处可用的**智能家居遥控器**与 **vibe-coding 语音输入器**，理想中的最小闭环体验包括：

1. **自带麦克风 + PTT（Push-to-Talk）**：按住机身 `Assistant` 键说话，松开即停，音频流直接送入本地或云端 ASR。
2. **交互反馈**：支持手柄震动反馈（如识别完成、指令执行成功/失败提示）以及可选的 3.5mm 耳机孔 TTS 语音回传。
3. **零路由器配置门槛**：普通用户不需要在家里路由器上折腾 DHCP Option 6（按 MAC 定向分配 DNS），也不需要理解 DNS Sink、TLS 证书劫持或主机共享热点等极客概念。

围绕这一目标，最自然会想到的三条“直接改造手柄或复用官方固件”的路径，但在技术上均存在硬性障碍。

---

## 2. 路径一：在原厂 Wi-Fi 固件的 BLE 配网阶段写入“代理服务器 / 自定义 DNS”？

### 2.1 BLE 配网协议不支持代理或静态网络参数

Stadia 控制器（Wi-Fi 固件 `ota.282115`，代号 Gotham）在未配网时会进入橙灯闪烁的 BLE Setup 模式，复用 Google Cast / Chromecast 系的 GATT 服务（`0000fea0-...`）。

向配网特征值 `CHAR_CONNECT`（`0328fe40-002f-11e5-87d0-0002a5d5c51b`）写入的 JSON 负载仅支持以下链路层参数：

```json
{
  "ssid": "<Wi-Fi 名称>",
  "wpa_auth": 7,
  "wpa_cipher": 4,
  "bssid": "<可选 AP MAC>",
  "frequency": 5180,
  "enc_passwd": "<RSA PKCS#1 v1.5 加密后的密码>"
}
```

- 控制器固件未在该 JSON 或 `CHAR_SETTINGS`（`0228fe40-...`）中实现 `http_proxy`、静态 IP 或自定义 DNS Server 字段。
- 连上 Wi-Fi 后，控制器完全依赖 **DHCP** 获取 IP、网关和 DNS；且当域名解析返回 `NXDOMAIN` 时，固件会绕过本地 DNS 直接回退到硬编码的 `8.8.8.8`。

### 2.2 即使存在 HTTP 代理，也无法覆盖完整的通信链路

控制器底层运行的是嵌入式 **lwIP** 网络栈与一个极简的串行 **HTTP/1.1** 客户端（无 HTTP `CONNECT` 隧道或 SOCKS 客户端逻辑）。更重要的是，手柄开机后的完整协议重度依赖 **UDP**，传统代理根本无法接管：

| 协议阶段 | 传输层协议与端口 | 作用 |
| :--- | :--- | :--- |
| 域名解析（DNS） | UDP / TCP `53` | 将 `*.googleapis.com` 与 `time.google.com` 引导至接收端 |
| 时间同步（SNTP） | UDP `123` | 向 `time.google.com` 同步时钟（用于签名/会话状态） |
| 发现与控制（HTTPS） | TCP `443` | `FirmwareVersionCheck`、`RegisterController`、`/gmsg` 长轮询等 |
| 媒体与输入（WebRTC） | UDP `40000+` | STUN/ICE、DTLS 1.2、SRTP（承载按键输入、震动、耳机音频与麦克风） |

因此，**不能通过蓝牙配网下发代理配置来解决 DNS 劫持与局域网路由问题**。

---

## 3. 路径二：改用官方蓝牙（BLE）模式 + WebHID / 桌面端读取麦克风？

Google 在 Stadia 停服后推出了永久性蓝牙转换工具（对应固件代号 `bruce`），使手柄可以作为标准无线蓝牙手柄直接配对 PC、Mac 或手机，且无需任何路由器配置。

但实测与协议分析表明，**官方蓝牙模式下无法获取手柄的任何音频数据**：

1. **蓝牙固件彻底禁用了音频子系统**
   - 官方蓝牙模式基于 **BLE（Bluetooth Low Energy）HOGP（HID over GATT）** 实现，仅包含低带宽的按键、摇杆、扳机输入与震动反馈输出。
   - 固件未实现经典蓝牙的 **HFP / HSP** 音频协议，也没有任何自定义 BLE 音频传输特征值（GATT Characteristic）。
   - 在无线蓝牙模式下，**机身内置麦克风与底部 3.5mm 耳机孔均被完全关闭**（3.5mm 音频口仅在 USB-C 有线连接或原厂 Wi-Fi 模式下工作）。
2. **WebHID 只能解决“扩展按键”问题，无法解决“麦克风”问题**
   - Chrome 官方技术博客《Talking to the Stadia Controller with WebHID》介绍的方案，是用 WebHID 读取原始 HID Report，以弥补浏览器标准 Gamepad API 只能识别 17 个常规按键（0–16）、读不到 **Assistant** 和 **Capture** 两个独立按键的缺陷。
   - 在官方蓝牙固件的 HID 描述符中，**不存在任何音频或麦克风端点**。即便通过 WebHID 或原生驱动监听到了 `Assistant` 键按下，也只能触发电脑桌面麦克风录音，无法利用手柄本身进行近场语音输入，手柄会退化成一个“必须守在电脑前才能说话的蓝牙遥控器”。

---

## 4. 路径三：通过逆向工程修改并刷写自定义 Wi-Fi / 蓝牙固件？

### 4.1 硬件架构：逻辑不在 Wi-Fi 芯片里，而在主控 MCU

Stadia 控制器（Model H2B）采用双芯片无线架构：

- **主控 MCU**：**NXP i.MX RT1061**（ARM Cortex-M7 跨界处理器）
- **无线射频芯片**：**Broadcom BCM43458**（802.11ac 双频 Wi-Fi + Bluetooth 5.0 LE）

BCM43458 仅负责射频与链路层收发；真正的 lwIP 协议栈、DNS 解析、TLS 证书校验、BLE 配网解析以及 Stadia 业务状态机全部运行在 **NXP i.MX RT1061** 的主固件中。`stadiacast` 项目本身正是通过逆向分析 `ota.282115` 固件提取出通信协议与 protobuf schema 的，因此在**代码定位与二进制修补层面并不存在不可逾越的障碍**。

### 4.2 决定性障碍：硬件级 Secure Boot（NXP HAB）

真正锁死自定义固件路线的是芯片级的安全启动机制：

1. **HAB（High Assurance Boot）不可绕过**
   - NXP i.MX RT1061 在出厂时已通过一次性可编程熔丝（**OTP e-fuse**）烧录了 Google 私钥对应的公钥哈希。
   - 每次设备从内部 Flash 冷启动时，不可篡改的 Boot ROM 都会强制对固件镜像进行密码学签名校验。
   - 社区现有的刷机工具（如 `stadiatool`）本质上也只能刷入 **Google 官方签名过的镜像**（例如在官方 Wi-Fi 固件与官方蓝牙固件之间切换）。**只要固件被修改 1 个字节（哪怕只是改一个硬编码域名或开启蓝牙麦），校验就会失败并拒绝启动。**
2. **已知漏洞均为非持久化（Tethered），无法用于无线场景**
   - 社区确实研究过针对 NXP Boot ROM USB **SDP（Serial Download Protocol）** 的溢出漏洞（如 `nxp-sdp-overrun`），可以在不拆机硬改的前提下通过 USB 在内存中引导未签名代码（例如运行移植版 Zephyr RTOS）。
   - 然而这类利用属于 **Tethered（有线临时注入）**：手柄一旦断电关机，下次唤醒时 Boot ROM 依然会检查 Flash 签名，必须再次插上 USB 线重新注入。这与“无线手持、随按随醒”的产品形态完全冲突。

---

## 5. 总结与架构推论

| 技术路线 | 能否使用手柄内置麦克风 | 能否脱离路由器极客配置 | 能否持久化无线冷启动 | 结论 |
| :--- | :---: | :---: | :---: | :--- |
| 原厂 Wi-Fi 固件 + BLE 写入代理 | 能 | 否（协议不支持代理/DNS） | 能 | 不可行 |
| 官方蓝牙固件（BLE HID / WebHID） | **不能** | 能 | 能 | 失去灵魂语音体验 |
| 逆向修改原厂固件 / 刷第三方固件 | 理论上能 | 理论上能 | **不能**（NXP HAB 锁死） | 无法用于无线产品 |
| **原厂 Wi-Fi 固件 + 专用无线接收器（Dongle）** | **能** | **能** | **能** | **唯一兼顾全部目标的最优解** |

**最终结论**：  
Stadia 手柄本身的软件栈是一座被硬件级 Secure Boot 封闭的“黑盒”，我们不应该也不可能去改造手柄固件；相反，应当**把手柄视为已定型的原厂终端设备**，通过一个独立、低成本、插电即用的**专用无线接收器（ESP32-S3 Wireless Adapter）**在物理层与网络层包办 Wi-Fi 热点、BLE 自动配网与 `stadiacast` 协议终结，从而彻底释放手柄原生麦克风、按键、震动和耳机回传的全部潜力。
