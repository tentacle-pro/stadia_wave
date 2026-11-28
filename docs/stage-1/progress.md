# Stage 1（模式一）开发进度追踪看板

> **锚定目标**：基于 **ESP32-S3-N16R8**（ESP-IDF v6.1.1），零第三方虚拟驱动，在 Windows / macOS 双平台实现 Stadia 手柄无线 **按键、麦克风、震动、耳机** 四大功能。  
> 详细技术评估与设计规范见同目录 [plan.md](./plan.md)。

---

## 1. 环境准备检查

- [x] 安装 ESP-IDF v6.1.1 CLI 工具链（`stadia_wave/esp32s3/prepare_runtime.sh`）
- [x] 确立 PoC 目录规范（`stadia_wave/esp32s3/stage-1_PoC-<N>`）与 Stage 1 文档体系
- [x] 确认本机 USB 连接 `ESP32-S3-N16R8` 开发板串口识别与最小 Hello World 烧录

---

## 2. 关键技术验证（PoC）进度

| 编号 | 目录 | 验证内容 | 状态 | 关键结论 / 阻塞点 |
| :--- | :--- | :--- | :---: | :--- |
| **PoC-1** | `esp32s3/stage-1_PoC-1` | DS4 HID（按键+震动）+ UAC 2.0（麦+耳机）双平台免驱与端点验证 | `macOS 通过（Win11 待补测）` | 4 IN / 3 OUT 端点预算通过；macOS 免驱识别按键/震动/麦克风/扬声器全通 |
| **PoC-2** | `esp32s3/stage-1_PoC-2` | BLE 自动配网 + Wi-Fi SoftAP 共存（真机手柄连入） | `真机通过` | NimBLE + SoftAP 共存稳定；成功读取 Window 1a、解压并 RSA 加密写入凭证，真机手柄连入并获配 `192.168.4.2` |
| **PoC-3** | `esp32s3/stage-1_PoC-3` | mbedTLS GTS 证书 + Clearcut SNI 秒拒 + DTLS-SRTP 密钥导出 | `真机通过` | DTLS-SRTP 60B 密钥导出自检通过；真机信任 GTS 证书链，Clearcut SNI 20ms 秒拒，成功推进至 `RegisterController`（457B） |
| **PoC-4** | `esp32s3/stage-1_PoC-4` | Core 1 Opus 全双工编解码（麦解码 + 耳机编码）+ UAC 压测 | `真机通过` | 240MHz + `-O2` 下立体声编码+单声道解码仅占 Core 1 **39.0%**（7.79ms/20ms），Live 全双工 50fps 零丢帧，空闲堆 8.6MB，四功能全保留 |

---

## 3. 正式实施里程碑（MVP 合龙）

- [x] **Step 1**：主固件骨架 + PSRAM 配置 + `nanopb` 生成 `.proto` + 嵌入 GTS `identity.bin`（`stadia_wireless_adapter.bin` 编译通过，`0x2ed40` 字节）
- [ ] **Step 2**：离线网关基础层（SoftAP + 泛解析 DNS + SNTP + 一键 BLE 配网）
- [ ] **Step 3**：HTTPS 信令 + Clearcut SNI 拦截 + Discovery & `/gmsg` 长轮询
- [ ] **Step 4**：WebRTC 数据面（STUN + DTLS + SRTP + WireHeader2）+ Display Link READY & 按键读取
- [ ] **Step 5**：Game Session 震动看门狗 + 麦克风 Opus 上行 + 3.5mm 耳机 Opus 下行
- [ ] **Step 6**：USB 复合设备四功能合龙 + macOS / Windows 11 全功能联调验收

---

## 4. 调试日志与关键决策记录

- **2026-11-28**：
  - 确立 Stage 1 四大功能范围：**按键、麦克风、震动、耳机**。
  - 确立无需 ViGEmBus / OpenJoystickDriver / VB-CABLE / BlackHole，全部由物理 USB（DS4 HID + UAC 2.0）双平台免驱承载。
  - 确立 USB 端点红线：四功能全开后不挂载 3 端点 CDC-ACM，调试走 UART0/JTAG，模式控制走 HID Feature Report。
  - **PoC-1 macOS 真机验收通过**：双口开发板通过 `USB/OTG` 跑复合设备、`UART (CH343)` 看日志；macOS 免驱跑通 DS4 摇杆/按键、Test Vibration 震动回调、`StadiaWave Audio` 48kHz 麦克风上行与扬声器下行（Windows 11 待后续补测）。
  - **PoC-2 真机验收通过**：NimBLE 扫描连接真机手柄、读取 Window `1a`（4459B → gzip 解压 5016B）、IDF v6.1.1 PSA Crypto RSA PKCS#1 v1.5 加密写入 SoftAP 凭证，手柄成功断开 BLE 并连入 `StadiaWave-PoC` 获配 `192.168.4.2`。
  - **PoC-3 真机验收通过**：DTLS 1.2 + `use_srtp` 60B 密钥导出自检通过；真机手柄信任 `identity.bin` 3 级 GTS 证书链，Clearcut `play.googleapis.com` 被 SNI 回调在 20ms 内秒拒，成功发出 `FirmwareVersionCheck` 与 `RegisterController`（457 字节）。
  - **PoC-4 真机压测通过**：ESP32-S3-N16R8（240MHz, `-O2`）运行 `2ch` 耳机 Opus 编码 + `1ch` 麦解码合计仅 **7.79ms / 20ms（39.0% Core 1）**，Live UAC 全双工 50fps 零丢帧且剩余堆内存 8.6MB；**确定保留四功能全双工方案**。
  - **全局工程规范备忘**：所有含 `mbedtls` / `Opus` 的任务必须给足栈空间（`main_task` ≥ 16KB，Opus 任务 ≥ 64KB），生产构建需开启 `CONFIG_COMPILER_OPTIMIZATION_PERF=y`（`-O2`）和 `4MB` 自定义分区表。
  - **Step 1 完成**：`stage-1_firmware` 主工程骨架搭建完成，7 份 Stadia `.proto`/`.descriptor` 全部通过 `nanopb`（启用 `PB_FIELD_32BIT=1` 兼容 `1000000+` 扩展 tag）生成 C 代码并验证编译链接通过。
  - **Step 2 & Step 3 完成（编译通过，待真机联调）**：
    - 整合离线网关基座：`wifi_ap`（`StadiaWave-PoC` / `stadia1234`，CH6 WPA2-PSK CCMP）、`dns_sntp`（通配符 DNS `:53` + SNTP `:123`）及 `ble_provisioner`（BOOT 键 `GPIO0` 一键触发 BLE 扫描与配网）。
    - 实现 `session_signaling`（`nanopb` 编解码 `FirmwareVersionCheck`、`RegisterController`、`DiscoverDisplay`、`GmsgPollRequest`/`GmsgEnvelope` 扩展 `269197544`、`ResolveDiscovery` 以及 `WebRtcSdpAndIceCandidates` Offer 解析与 Answer `192.168.4.1:40000` 生成）。
    - 升级 `https_server` 为每连接独立 FreeRTOS 任务并发模型（避免 `/gmsg` 长轮询阻塞 `ResolveDiscovery` 与 Clearcut SNI 拒绝），`idf.py build` 编译通过（`stadia_wireless_adapter.bin` = `0x10fe30` bytes）。
  - **Step 4 完成（`rtc_endpoint` 编译通过，待真机联调）**：
    - 实现 [rtc_endpoint.c](file:///Users/dio/workspace/stadia_wave/esp32s3/stage-1_firmware/main/rtc_endpoint.c)：开机自动计算证书 SHA-256 指纹注入 `session_signaling`；UDP `:40000` 复用 ICE-lite STUN Binding Response（`XOR-MAPPED-ADDRESS` + `MESSAGE-INTEGRITY` HMAC-SHA1 + `FINGERPRINT` CRC32）与 DTLS 1.2 Server（`use_srtp` `SRTP_AES128_CM_HMAC_SHA1_80`，`raw_dtls` WireHeader2 8 字节帧头解析与发送队列）。
    - 已接入 `main.c` 并通过 `idf.py build` 编译（`stadia_wireless_adapter.bin` = `0x1110c0` bytes）。
  - **Step 5 完成（`display_rpc` 编译通过，待真机联调）**：
    - 真机验证突破 `STUN` → `DTLS 1.2 Handshake Complete` → `ResolveDiscovery received` 里程碑。
    - 实现 [display_rpc.c](file:///Users/dio/workspace/stadia_wave/esp32s3/stage-1_firmware/main/display_rpc.c)：`ResolveDiscovery` 到达后发送 `GetControllerStateRequest` 进入 `READY` 状态，打印硬件信息与耳机插拔/电量通知，响应手柄 `GetCurrentTimeRequest`（原样回显 `controller_time_us`）与 `keepalive`，并实时解码按键/摇杆/扳机输入（`ControllerToDisplay #3`）。
    - 已接入 `main.c` 并通过 `idf.py build` 编译（`stadia_wireless_adapter.bin` = `0x112d30` bytes）。
  - **Step 6A 完成（USB DS4 HID + UAC 2.0 桥接编译通过，待上机实测）**：
    - 真机验证 `Display Link READY`（`GA00827-US`, `fw=282115`）并实时收到 Stadia 摇杆/按键输入。
    - 实现 [usb_bridge.c](file:///Users/dio/workspace/stadia_wave/esp32s3/stage-1_firmware/main/usb_bridge.c)：初始化 DualShock 4 HID + UAC 2.0 复合设备，将 Stadia 按键/D-pad/摇杆/L2R2 实时映射为 64 字节 DS4 输入报告（125Hz），并接收主机震动输出报告。
    - 已接入 `main.c` 并通过 `idf.py build` 编译（`stadia_wireless_adapter.bin` = `0x1194b0` bytes）。
  - **Step 6B-1 完成（GameSession + 震动闭环编译通过，待实测）**：
    - 真机上 Mac 网页手柄测试通过（免驱 DS4 识别与输入全部正常）。
    - 实现 [game_session.c](file:///Users/dio/workspace/stadia_wave/esp32s3/stage-1_firmware/main/game_session.c)：`Display Link READY` 后自动发送 `StartGame`，处理手柄 `ProxyRequest`（`GenerateInitialization`），在 UDP `:40002` 建立 GameSession ICE/DTLS 通道，并将主机下发的 DS4 `0x05` 震动报告映射为 `InputFeedbackMessage_V2.gamepad_vibration`（`0..65535`，带 2 秒看门狗保活）驱动手柄马达震动。
    - 已接入 `main.c` 并通过 `idf.py build` 编译（`stadia_wireless_adapter.bin` = `0x11bba0` bytes）。
