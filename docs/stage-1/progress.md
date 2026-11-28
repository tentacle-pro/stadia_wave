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
| **PoC-4** | `esp32s3/stage-1_PoC-4` | Core 1 Opus 全双工编解码（麦解码 + 耳机编码）+ UAC 压测 | `待真机实测` | 已完成开发并编译通过（`0x602e0` 字节），含 240MHz 基准测试（1ch/2ch Enc + 1ch Dec）与 UAC 实时 Opus 环回 |

---

## 3. 正式实施里程碑（MVP 合龙）

- [ ] **Step 1**：主固件骨架 + PSRAM 配置 + `nanopb` 生成 `.proto` + 嵌入 GTS `identity.bin`
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
