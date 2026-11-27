# ESP32-S3 无线接收器工作模式、协议设计与实施路线

> 本文界定 `stadia_wave` 配套 **ESP32-S3-N16R8 Wireless Adapter** 的三大产品工作模式，给出各模式下的 USB / 网络传输协议设计，并确立 **“模式一 → 模式二 → 模式三”** 的分阶段工程实施路线。

---

## 1. 三大工作模式概览

| 维度 | 模式一：标准无线 HID 手柄 | 模式二：`stadia_wave` USB 直传伴侣 | 模式三：独立供电局域网桥接 |
| :--- | :--- | :--- | :--- |
| **供电与连接** | 插在目标主机 USB 口 | 插在目标主机 USB 口 | 仅接 5V USB 充电头 |
| **USB 枚举形态** | 标准 HID 手柄（+ 可选 UAC 麦）+ CDC 控制口 | **隐藏/关闭 HID**，仅保留 CDC / Vendor 数据口 | 无 USB 数据通信 |
| **无线拓扑** | `手柄 <--(2.4G SoftAP)--> ESP32` | `手柄 <--(2.4G SoftAP)--> ESP32` | `手柄 <--(SoftAP)--> ESP32 <--(STA)--> 路由器` |
| **事件消费方** | 操作系统 / 游戏 / Steam / 浏览器 | 本机 `stadia_wave` App（再映射为 OS 操作） | 指定主机 `stadia_wave`（WS）/ Home Assistant（MQTT） |
| **端到端延迟** | 极低（1 跳无线 + USB） | 极低（1 跳无线 + USB） | 中低（2 跳无线：经家庭路由器转发） |
| **核心价值** | 最大化游戏与应用兼容性 | 避免系统抢占手柄，极致响应的桌面控制与语音输入 | 脱离主机 USB，实现全屋智能家居中控与跨设备触发 |

---

## 2. 模式一：标准无线 HID 手柄模式（Milestone 1）

### 2.1 目标与定位
将 ESP32-S3 打造为即插即用的“Stadia 版 Xbox 无线接收器”。无论用户是否安装 `stadia_wave`，插上 Windows / macOS / Linux / Steam Deck 即可识别为标准物理游戏手柄。

### 2.2 设备接口设计
- **USB HID Gamepad**：
  - 上报标准手柄输入（D-Pad、ABXY、L1/R1、L2/R2 线性扳机、左右摇杆、L3/R3、Menu/Options、Stadia Home 键），并映射或通过扩展 Usage 上报 `Assistant` 与 `Capture` 键。
  - 接收宿主机下发的 HID Output Report / Force Feedback（震动指令），转换为 `stadiacast` Game Session 的 SRTP 震动控制包发给手柄。
- **USB UAC 麦克风（可选开关）**：
  - 在 Core 1 上将手柄发来的 48kHz Mono Opus 解码为 PCM，以标准免驱 USB 麦克风暴露给操作系统（也可按需关闭以节省算力）。
- **USB CDC 控制口（轻量常驻）**：
  - 保留一个极简串口通道，供 `stadia_wave` 或网页工具查询 Dongle 状态、触发 BLE 配对以及下发切换到模式二的指令。

---

## 3. 模式二：`stadia_wave` USB 直传伴侣模式（Milestone 2）

### 3.1 为什么需要隐藏标准 HID？
当用户用 Stadia 手柄做 **vibe-coding 语音输入** 或 **桌面应用快捷键编排** 时，如果系统同时将其识别为标准游戏手柄，后台运行的 Steam、浏览器或游戏窗口会抢占或误响应按键。因此模式二下**必须关闭标准 HID 输出**，让所有按键与语音事件独占直达 `stadia_wave`。

### 3.2 动态切换机制（模式一 ↔ 模式二）
- **控制入口**：
  1. **USB 控制 API（优先）**：`stadia_wave` 启动或用户切换场景时，直接通过 USB CDC / Control Transfer 下发 `set_mode` 指令。
  2. **内置 Web / 串口工具兜底**：通过 WebSerial（或本地控制页）手动切换。
- **切换行为**：
  - 收到切换指令后，ESP32-S3 将目标模式写入 **NVS Flash**（掉电记忆），并触发 TinyUSB 执行一次 **USB 软断开重枚举（Re-enumeration）**。
  - 切至模式二时，USB 描述符中**移除 HID Gamepad 接口**，仅暴露高速数据/控制接口；切回模式一时重新挂回 HID 描述符。

### 3.3 ESP32 ↔ `stadia_wave` USB 帧协议设计

为同时承载高频按键报文与 20ms 一帧的 Opus 语音流，USB CDC / Vendor Bulk 通道采用紧凑的二进制定界帧（或 COBS / SLIP 封装，若走 Vendor Bulk 则直接依赖 USB 包边界）：

```text
+--------+---------+----------+------------+------------------+--------+
| Magic  | Version | MsgType  | PayloadLen | Payload          | CRC16  |
| 2 Byte | 1 Byte  | 1 Byte   | 2 Byte(LE) | N Bytes          | 2 Byte |
| 0x5357 | 0x01    | uint8    | uint16     | protobuf / raw   | CCITT  |
+--------+---------+----------+------------+------------------+--------+
```

| 方向 | `MsgType` | 名称 | 载荷说明 |
| :--- | :--- | :--- | :--- |
| **上行 (ESP32 → Host)** | `0x01` | `INPUT_STATE` | 手柄完整状态快照（32-bit 按键位图含 `Assistant`/`Capture`、双摇杆 int16、双扳机 uint8、时间戳）。 |
| **上行 (ESP32 → Host)** | `0x02` | `AUDIO_OPUS_FRAME` | **裸 Opus 帧透传**（48kHz Mono, 20ms, 约 90–280 字节）。ESP32 不做解码，由 `stadia_wave` 解码送 ASR，CPU 开销近乎为零。 |
| **上行 (ESP32 → Host)** | `0x03` | `CONTROLLER_STATUS` | 手柄上下线状态、电量百分比、3.5mm 耳机插拔状态、MAC/Device ID。 |
| **下行 (Host → ESP32)** | `0x81` | `SET_WORK_MODE` | 切换工作模式（`0x01`=HID 模式，`0x02`=直传模式，`0x03`=桥接模式）及重枚举选项。 |
| **下行 (Host → ESP32)** | `0x82` | `SET_RUMBLE` | 左/右马达强度（uint16）及持续时间（ms），用于 `stadia_wave` 提供识别成功/失败触觉反馈。 |
| **下行 (Host → ESP32)** | `0x83` | `SET_MIC_STATE` | 请求手柄开启/关闭麦克风采样（可由 `stadia_wave` 在收到 `Assistant` 按下后联动，或由 ESP32 本地 PTT 直接触发）。 |
| **下行 (Host → ESP32)** | `0x84` | `AUDIO_TTS_OPUS` | （可选）PC 端编好的 Opus 语音帧，ESP32 按 20ms 节拍封装 SRTP 播报至手柄 3.5mm 耳机。 |
| **双向控制** | `0xF0` | `DEVICE_CONFIG` | JSON 配置读写（如查询固件版本、设置桥接目标、触发 BLE 配对扫描）。 |

---

## 4. 模式三：独立供电局域网桥接模式（Milestone 3）

### 4.1 运行形态
ESP32-S3 仅接 5V USB 充电头，启用 **Wi-Fi AP+STA 共存**：
- **SoftAP**：维持固定或自定义的离线局域网，接管 Stadia 手柄的 `stadiacast` 全栈协议。
- **STA**：连接家庭主路由器，作为主动客户端将手柄事件推送到外部目标。

### 4.2 转发目标与协议配置
该模式下，事件“发给谁、用什么协议、带什么数据”完全由 ESP32 内部存储的 **路由配置表（NVS）** 决定，直接复用模式二定义好的事件与音频模型：

1. **目标类型 A：`stadia_wave` 远程主机（WebSocket）**
   - 配置目标 URL（如 `ws://192.168.1.215:9400/ws/controller`）。
   - 支持直接通过 WebSocket Binary Frame 发送与模式二完全一致的 `INPUT_STATE` / `AUDIO_OPUS_FRAME` 二进制帧，并接收后端下发的 `SET_RUMBLE` 反馈。
   - **性能预期**：因需经过家庭路由器做一次 Wi-Fi 中继，延迟会高于模式二的 USB 直连，但适合无线距离跨房间或主机无空闲 USB 口的场景。
2. **目标类型 B：智能家居平台（MQTT / HTTP Webhook）**
   - 配置 MQTT Broker 地址、Topic 前缀与认证信息（或 Webhook URL）。
   - 在 ESP32 端将原始按键映射为语义化 JSON 事件（如 `{"button": "assistant", "action": "hold"}`）或直接转发短语音流至家庭 ASR 网关，实现“电脑关机也能控全屋家电”。

### 4.3 内置 Web Server 与配置 API
由于独立供电时没有主机 USB 可用，ESP32-S3 在桥接模式下内置轻量 HTTP Server + REST API（同时对 STA 侧局域网或按键触发的临时热点开放，带简易 PIN/Token 保护）：
- `GET /api/status`：查看手柄连接状态、电量、SoftAP/STA 信号与当前模式。
- `GET / PUT /api/config/routing`：配置转发协议（WebSocket / MQTT / Webhook）、目标 IP/端口、以及携带数据类型（仅按键 / 按键 + 语音）。
- `POST /api/mode`：远程切换工作模式或重启设备。
- `POST /api/ble-pair`：触发手柄 BLE 自动配对。
- **无电脑初次配网**：长按外壳物理按键 3 秒开启 2 分钟临时热点，手机（含 iPhone）通过系统原生 **Captive Portal** 弹窗完成家庭 Wi-Fi 与目标地址配置。

---

## 5. 工程实施路线图（为什么按 一 → 二 → 三 推进）

按 **模式一 → 模式二 → 模式三** 的顺序推进，每个阶段都能产出可独立验证、对真实用户有用的完整成果，且下游直接复用上游协议资产：

### 阶段 1：模式一（核心无线协议栈 + 标准 HID MVP）
- **核心任务**：
  - 移植 `stadiacast` 全离线闭环（SoftAP + 泛解析 DNS + SNTP + mbedTLS/GTS + Clearcut SNI 拦截 + Discovery/GMsg + STUN/DTLS/SRTP + nanopb）。
  - 实现物理按键触发的 BLE GATT 自动配网。
  - 实现 TinyUSB 标准 HID Gamepad 上报与震动下发。
- **阶段验收**：不装任何上位机软件，插上 Win/Mac/Linux 即可用浏览器手柄测试页看到所有按键摇杆响应及震动，并能用网页一键给新账号手柄配对。

### 阶段 2：模式二（`stadia_wave` USB 伴侣协议 + 语音直传）
- **核心任务**：
  - 实现 TinyUSB 动态重枚举切换（隐藏/开启标准 HID）。
  - 实现第 3.3 节定义的 USB 帧协议（`INPUT_STATE`、`AUDIO_OPUS_FRAME`、`SET_RUMBLE`、`SET_WORK_MODE`）。
  - 在 `stadia_wave` 桌面端接入 USB 通道，打通“按住 `Assistant` 键说话 → 裸 Opus 送 ASR → vibe-coding 输入 / OS 动作 → 手柄震动反馈”的低延迟闭环。
- **阶段验收**：在 Mac/Windows 上插上 Dongle 运行 `stadia_wave`，Steam/游戏不再误识别手柄，语音按键编排延迟达到最佳体感。

### 阶段 3：模式三（独立供电桥接 + 内置 Web 配置服务）
- **核心任务**：
  - 开启 ESP-IDF `AP+STA` 共存，封装 WebSocket Client 与 MQTT Client 转发引擎（直接复用阶段 2 的二进制/JSON 事件载荷）。
  - 开发内置 Web Server 管理界面及 REST 配置 API，补齐“长按进入临时热点 + Captive Portal”手机配网流程。
  - 实测 2.4GHz AP+STA 同频转发下的按键与音频抖动，优化缓冲与信道策略。
- **阶段验收**：Dongle 仅插 5V 充电头，可在脱离电脑的情况下控制 Home Assistant，并可通过 Wi-Fi WebSocket 远程接入指定主机上的 `stadia_wave`。
