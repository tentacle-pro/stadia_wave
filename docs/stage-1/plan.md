# Stage 1（模式一）技术评估与实施蓝图

> **阶段定位**：将 **ESP32-S3-N16R8** 打造为类似“Xbox Controller Wireless Adapter”的 **Stadia 2.4GHz 无线手柄接收器**，在 Windows / macOS / Linux 上**零第三方虚拟驱动**实现 **按键、麦克风、震动、耳机** 四大完整功能。

---

## 1. 开发环境与目录规范

- **硬件目标**：`ESP32-S3-N16R8`（16MB Flash + 8MB Octal PSRAM，双核 240MHz，原生 USB-OTG）
- **工具链版本**：ESP-IDF **v6.1.1**（纯 CLI 方式）
- **工程根目录**：`stadia_wave/esp32s3/`
- **每次新开 Shell 必做命令**：
  ```sh
  cd /Users/dio/workspace/stadia_wave/esp32s3
  source ./prepare_runtime.sh        # 必须 source，确保在当前 shell 激活 IDF v6.1.1 环境
  ```
- **每个新建 ESP-IDF 工程必做初始化**：
  ```sh
  idf.py set-target esp32s3
  ```
- **PoC 目录命名规范**：
  - `stadia_wave/esp32s3/stage-1_PoC-1`（USB 复合设备双平台免驱验证）
  - `stadia_wave/esp32s3/stage-1_PoC-2`（BLE 自动配网 + Wi-Fi SoftAP 共存验证）
  - `stadia_wave/esp32s3/stage-1_PoC-3`（mbedTLS SNI 秒拒 + DTLS-SRTP 握手验证）
  - `stadia_wave/esp32s3/stage-1_PoC-4`（Opus 双向编解码 + UAC 双向音频压测）

---

## 2. 核心评估一：双平台全功能是否还需要 ViGEmBus / OpenJoystickDriver / 虚拟声卡？

**结论：完全不需要安装任何第三方虚拟驱动（0 驱动依赖）。**

软件版 `stadiacast` 需要 ViGEmBus（Windows）、OpenJoystickDriver（macOS）、VB-CABLE（Windows 虚拟麦）与 BlackHole（macOS 虚拟声卡），仅是因为纯软件无法向系统注册物理设备。ESP32-S3 作为真实物理 USB 硬件，可直接通过标准 USB 协议在双平台免驱工作：

| 功能 | USB 协议形态 | Windows 10/11 | macOS 12+ | 备注 |
| :--- | :--- | :---: | :---: | :--- |
| **按键输入** | **DualShock 4 USB HID**（默认） / Xbox 360 XInput（可选） | 原生免驱 | **DS4 原生免驱**（有线 XInput 不支持） | 默认首选 DS4 HID，覆盖全按键并兼顾 macOS；后续可增设 XInput Profile 兼容 Windows 老游戏。 |
| **震动反馈** | DS4 HID Output Report → Game Session SRTP | 原生免驱 | 原生免驱（GameController / SDL / Chrome） | 需在 ESP32 上维持 2s 震动看门狗保活（手柄固件 5s 无更新会自动停转）。 |
| **麦克风输入** | **USB UAC 2.0 麦克风（Isochronous IN）** | 原生免驱 | 原生免驱（开启 `UAC_SUPPORT_MACOS`） | 手柄 48kHz Mono Opus → ESP32 解码 PCM → 系统标准 USB 麦克风。 |
| **3.5mm 耳机输出** | **USB UAC 2.0 扬声器（Isochronous OUT）** | 原生免驱 | 原生免驱（开启 `UAC_SUPPORT_MACOS`） | 系统音频 48kHz PCM → ESP32 编码 Opus → `SetAudioMode(mode=2)` + Display Link SRTP 播放至手柄耳机孔。 |

---

## 3. 核心评估二：ESP32-S3 硬件硬约束（USB 端点与双向音频算力账本）

### 3.1 USB 物理端点硬限制（关键避坑点）
ESP32-S3 的 USB-OTG（Synopsys DWC2）外设最多支持 **EP0 + 6 个可配置端点**，且受 TX FIFO 限制，**同时活跃的 IN 端点上限为 5 个（含 EP0 IN）**。

四功能全开时的精确端点账本：
- `EP0`（控制）：**1 IN** + 1 OUT
- `HID Gamepad`（DualShock 4 按键 + 震动）：**1 Interrupt IN** + 1 Interrupt OUT
- `UAC 2.0 双向音频`（麦克风 + 耳机扬声器）：**1 Isochronous IN**（麦）+ 1 Isochronous OUT（耳机）+ （可选 **1 Isochronous IN** 异步反馈端点）
- **总计**：**3 ~ 4 个 IN 端点，3 个 OUT 端点** —— **完全满足 5 IN / 7 总端点的硬件限制**。

> **设计红线**：模式一四功能全开时，**不要再额外挂载标准的 3 端点 CDC-ACM 虚拟串口**（否则会触发 `dcd_edpt_open` 端点耗尽报错）。模式一所需的极简控制指令（如查询状态、切换至模式二）应复用 **HID Feature Report / Vendor HID Report（0 额外端点）**；调试日志直接走 ESP32-S3 的独立 **UART0 / USB-Serial-JTAG** 口即可。

### 3.2 Opus 双向编解码算力分配（Core 0 / Core 1 隔离）
- **Core 0**：专职 Wi-Fi SoftAP、lwIP、DNS/SNTP、mbedTLS（HTTPS/DTLS）、SRTP、nanopb 与 BLE 配网。
- **Core 1**：专职音频与 USB 实时流水线：
  - **上行麦克风解码**：48kHz Mono Opus (64kbps, 20ms) → PCM，ESP32-S3 单核占用约 15%–25%，非常轻松。
  - **下行耳机编码（重点压测项）**：PC UAC 扬声器 48kHz PCM → Opus 编码 → SRTP。Opus 编码比解码重得多，需在 **PoC-4** 中重点实测 `OPUS_APPLICATION_RESTRICTED_LOWDELAY` + 低复杂度（`complexity = 0..2`）下，48kHz 立体声 vs 单声道编码的 CPU 占用率，确定在“双向全双工（同时开麦 + 听耳机）”下不丢帧的最优参数。

---

## 4. 核心评估三：C/C++ 复刻 `stadiacast` 工作量与 ESP-IDF 组件清单

### 4.1 协议栈移植工作量估算
`stadiacast` 核心网络与协议代码共约 **7,263 行 Python**；剔除多手柄管理、CLI、系统驱动封装后，ESP32-S3 单手柄核心实现约 **3,200–4,200 行 C/C++**：

| 模块 | Python 参考代码 | C/C++ 预估 | 难度与重点 |
| :--- | :--- | :---: | :--- |
| BLE 自动配网 | `ble_provisioner.py` (619 行) | ~450 行 | 中：NimBLE GATT Client + 512B 分页窗口 + gzip 解压 + RSA PKCS#1 v1.5 |
| 离线 DNS + SNTP | `dns_sink/dnswire/sntp` (553 行) | ~250 行 | 低：泛解析固定回 `192.168.4.1`，AAAA 回空，SNTP 回本地时钟 |
| HTTPS + SNI + Discovery | `http_server/receiver/signaling` (1,824 行) | ~900 行 | 中：mbedTLS 443 + `mbedtls_ssl_conf_sni` 秒拒 `play.googleapis.com` + `/gmsg` 长轮询 |
| WebRTC 数据面 | `rtc/*` (1,438 行) | ~1,100 行 | **高**：STUN + mbedTLS DTLS 1.2 (`use_srtp`) + SRTP (`AES128_CM_SHA1_80`) + RTP + WireHeader2 |
| RPC 状态机 + Protobuf | `controller/game_session/protos/input` (2,729 行) | ~800 行 | 中：`nanopb` 直接编译现有 `.proto`，实现 READY / StartGame / 震动看门狗 / 麦与耳机控制 |
| TinyUSB 复合设备 + 音频 | 新增 | ~700 行 | 中高：DS4 HID（按键+震动）+ UAC 2.0 双向音频（麦+耳机）+ Opus 双向编解码 |

### 4.2 ESP-IDF v6.1.1 官方与社区组件拼装清单

| 能力 | 选用组件 | 来源 | 关键配置项 |
| :--- | :--- | :--- | :--- |
| Wi-Fi SoftAP + DHCP + lwIP | `esp_wifi`, `esp_netif`, `lwip` | ESP-IDF 内置 | WPA2-PSK (AES/CCMP)，PMF 关闭 |
| BLE GATT Client + 共存 | `bt` (NimBLE) | ESP-IDF 内置 | `CONFIG_ESP_COEX_SW_COEXIST_ENABLE=y` |
| TLS 1.2 / SNI / DTLS-SRTP / RSA | `mbedtls` | ESP-IDF 内置 | `CONFIG_MBEDTLS_SSL_PROTO_DTLS=y`、`CONFIG_MBEDTLS_SSL_DTLS_SRTP=y`、启用 PSRAM 分配 |
| Gzip 解压 | `miniz` / `zlib` | ESP-IDF 内置 | 用于解压 BLE Window `1a` 加密包 |
| Protobuf | `nanopb` | 社区标准组件 | 直接复用 `stadiacast/src/stadiacast/schemas/*.proto` |
| Opus 编解码 | `espressif/esp_audio_codec` 或 `esphome/micro-opus` | ESP Registry | 启用 ESP32-S3 SIMD 加速与 PSRAM 缓冲 |
| USB 复合设备（HID + UAC 双向） | `espressif/esp_tinyusb` + `espressif/usb_device_uac` | ESP Registry | 开启 `UAC_SUPPORT_MACOS`，参考 GP2040-CE 的 DS4 HID 描述符 |

---

## 5. 关键技术验证（PoC）清单（先排雷，再合龙）

| PoC 编号与目录 | 验证目标 | 验收通过标准 |
| :--- | :--- | :--- |
| **PoC-1**<br>`esp32s3/stage-1_PoC-1` | **USB 复合设备双平台免驱验证**（DS4 HID 按键/震动 + UAC 2.0 麦克风 + UAC 2.0 耳机） | 在 **macOS 与 Windows 11** 均免驱枚举无端点报错；浏览器 Gamepad Tester 正常读按键并触发震动回调；系统能录制 ESP32 440Hz 测试音且能将电脑音频播放到 ESP32 UAC 扬声器回调。 |
| **PoC-2**<br>`esp32s3/stage-1_PoC-2` | **BLE 自动配网 + Wi-Fi SoftAP 共存验证** | 按下开发板按键触发 NimBLE 扫描橙灯 Stadia 手柄，成功读 RSA 公钥并写入 SoftAP 凭证，手柄断开 BLE 并连入 ESP32 SoftAP 获取 IP。 |
| **PoC-3**<br>`esp32s3/stage-1_PoC-3` | **mbedTLS SNI 秒拒 + DTLS-SRTP 握手验证** | 使用内置 GTS `identity.bin` 证书，验证 `play.googleapis.com` 在 ClientHello 阶段被回 `handshake_failure` alert 秒拒；验证 DTLS 1.2 + `use_srtp` 握手并成功导出 SRTP 密钥。 |
| **PoC-4**<br>`esp32s3/stage-1_PoC-4` | **Opus 全双工编解码 + UAC 实时压测** | Core 1 同时运行 48kHz Mono Opus 解码（麦）与 48kHz 低复杂度 Opus 编码（耳机），Core 0 模拟 Wi-Fi UDP 持续收发，实测 CPU 占用、延迟、PSRAM 消耗与连续 5 分钟无爆音断流。 |

---

## 6. 真实实施执行步骤拆分（MVP 合龙）

1. **Step 1：工程骨架与 Protobuf 生成**
   - 创建 Stage 1 主固件工程（`esp32s3/stage-1_firmware`），配置 `ESP32-S3-N16R8`（16MB Flash、8MB Octal PSRAM、mbedTLS PSRAM）。
   - 接入 `nanopb` 生成 7 份 `.proto` 的 C 编解码器，将 `identity.bin` 嵌入 Flash。
2. **Step 2：离线网关基础层（SoftAP + DNS + SNTP + BLE 配网）**
   - 集成 PoC-2 的 SoftAP、泛解析 DNS、极简 SNTP 与物理按键一键 BLE 配网。
3. **Step 3：HTTPS 信令与 Discovery 状态机**
   - 集成 PoC-3 的 mbedTLS 443 服务 + Clearcut SNI 秒拒，实现 `generate_204`、`stadiaconfig`、`Omaha`、`FirmwareVersionCheck`、`RegisterController`、`/gmsg` 长轮询与 `ResolveDiscovery`。
4. **Step 4：WebRTC 数据面（STUN + DTLS + SRTP + Display Link）**
   - 实现 UDP 多路复用、ICE 保活、DTLS 1.2 + SRTP 与 WireHeader2 拆包；打通 `GetControllerState` → `READY` → `GetCurrentTime` 回显 → 实时收到手柄按键/电量/耳机插拔事件。
5. **Step 5：Game Session 震动 + 双向音频链路**
   - 打通 `StartGame` 与 Game Session UDP 会话，实现 2s 震动看门狗刷新；
   - 打通 `set_microphone`（支持 `Assistant` 键 PTT 或常开）与 `SetAudioMode(mode=2)` 耳机播放流（独立 SSRC、20ms 节拍、`slot * 960` 时间戳）。
6. **Step 6：USB 复合设备合龙与双平台稳定性联调**
   - 将按键、震动、麦克风、耳机四条数据通路完整接入 PoC-1 的 DS4 HID + UAC 2.0 复合设备；
   - 在 macOS（Apple Silicon）与 Windows 11 完成冷插拔、手柄休眠唤醒、同时按键 + 震动 + 语音录音 + 耳机播报的全功能长稳测试。
