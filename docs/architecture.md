# Stadia Wave 技术方案

首版目标是让 Wave Bridge 和 Wave Companion 在当前 macOS 上通过 DNS-SD 发现和 WebSocket 通信，完成录音转写与震动测试。产品流程和验收条件见[产品文档](product.md)。

## 代码组织与依赖

一个 Rust workspace 构建两个程序。Bridge 使用 Rust 实现后台服务和命令行入口。Companion 使用 Rust 和 Native Shell（NSH）实现桌面界面。菜单栏常驻、浮窗焦点、辅助功能和 Keychain 通过 macOS 适配层处理，具体接入方式在原型中验证。共享 crate 放置协议类型和版本。 [NSH 开发文档](https://nsh.dev/docs/)

```text
apps/bridge/       Stadiacast 子进程、发现发布、WebSocket 服务
apps/companion/    桌面界面、发现浏览、WebSocket 客户端、ASR、文字输入
crates/protocol/   消息类型、版本、录音与请求标识
```

初版对接 Stadiacast 1.3.0，配置显式指定可执行文件。Bridge 启动时检查路径与运行参数，在用户数据目录为每次运行分配独立的 `--log-dir`。Stadia Wave 的构建无需相邻的 Stadiacast 和 StadiaEnhanced 仓库。

## 网络和发现

```text
手柄 ── Wi‑Fi ── Stadiacast
                     │ 本机事件 API、结构化日志、Ogg Opus
                     ▼
                  Wave Bridge ←── WSS ── Wave Companion
                     │                         ▲
                     └────── DNS-SD 发布 ──────┘
```

| 链路 | 建立方式 |
| --- | --- |
| 手柄 → Stadiacast | 路由器按手柄的 Wi‑Fi MAC 下发 DHCP Option 6，指向 Bridge 主机的固定 LAN IP。Stadiacast 在同一台主机提供手柄使用的 DNS 和接收服务。 |
| Companion → Bridge | Bridge 在 LAN 发布 `_stadia-wave._tcp.local.`。Companion 浏览服务、解析主机与端口，再通过发现到的地址建立 WSS。两者同机时也使用这套流程。 |
| Bridge → Stadiacast | Bridge 在本机通过回环地址访问 Stadiacast，读取状态、调用控制接口，并读取本次运行目录中的日志与录音。 |

DNS-SD 实例名用于显示设备名称，TXT 字段公布协议主版本。Companion 手动输入 Bridge 地址时复用同一套连接与认证逻辑。mDNS 发现依赖本地链路，发布和浏览权限按 macOS 局域网规则处理。 [DNS-SD](https://www.rfc-editor.org/rfc/rfc6763.html) · [macOS 局域网权限](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)

Bridge 持久保存自己的身份密钥。首次配对通过本机 CLI 提供短时有效的一次性凭据和证书指纹，Companion 从该可信渠道取得指纹，验证 WSS 服务端后再交换配对凭据。每个 Companion 保存自己的长期凭据，macOS 上使用 Keychain。重连验证的是配对身份，地址和发现记录用于定位。局域网客户端使用 Bridge 的认证接口。Stadiacast 自带的 HTTP 控制端口需要按实际监听范围限制访问，尤其是部署在多人共用的 LAN 时。 [WebSocket 协议](https://www.rfc-editor.org/rfc/rfc6455.html) · [Stadiacast 监听端口](https://github.com/jkfran/stadiacast/blob/v1.3.0/docs/setup.md#network)

## Bridge 运行和数据来源

Bridge 管理一个 Stadiacast 子进程。它复用当前已验证的 DNS 和网络参数，启用麦克风录音，并用 `--start-game` 准备震动通道。Bridge 持续读取 stdout 和 stderr 作为诊断。启动失败时展示可执行文件路径、端口冲突或进程退出等具体原因。正常关停时先走本机 `/_shutdown`，等待子进程退出。 [Stadiacast 启动和控制](https://github.com/jkfran/stadiacast/blob/v1.3.0/docs/setup.md#running)

| 数据或操作 | 接口及使用方式 |
| --- | --- |
| 手柄在线、按键当前状态和电量 | 首版每 500 毫秒读取 `GET /_status`，按键边沿由结构化日志提供。第一帧用于建立状态基线。后续可用 `/_events` 优化状态更新。 |
| 精确按键边沿 | 用同一运行目录的结构化 `input` 日志识别 A、B 等按键的按下与松开。快照只更新状态，避免状态和日志重复触发同一动作。 |
| PTT 与录音 | 复用 Stadiacast 的 Assistant PTT。用 `/_mic` 启用或停用 PTT，结合 `mic_switch`、`mic_recorded` 和失败事件管理录音生命周期。 |
| 完成的音频 | `mic_recorded` 在 Ogg 文件关闭后发出。只读取本次运行目录中与活动录音匹配的文件，有写入错误、空文件或取消时报告失败。 |
| 震动 | Bridge 通过本机 `POST /_rumble` 触发有限时长的脉冲。需要手柄在线和 game session 可用。 |

录音开始和结束关联到同一手柄与同一轮 PTT，并按日志顺序处理。Bridge 仅为当前会话接收录音。退出、掉线或接管时停用 PTT 并停止进行中的录音，录音归属失效后丢弃对应的晚到文件。新会话等待 Assistant 回到松开状态再接受下一次按下。Bridge 在仅当前用户可读的运行目录中管理音频，收到完整交付确认或判定录音丢弃后清理对应文件。 [Stadiacast 事件与麦克风](https://github.com/jkfran/stadiacast/blob/v1.3.0/docs/setup.md#running) · [录音文件关闭顺序](https://github.com/jkfran/stadiacast/blob/v1.3.0/src/stadiacast/microphone.py) · [震动协议](https://github.com/jkfran/stadiacast/blob/v1.3.0/docs/protocol.md#6-the-game-session-schemasstreamer_messageproto-game_sessionpy)

## 单连接与接管

Bridge 维护唯一的活动 WebSocket。Bridge 在接管前校验 WebSocket 升级请求、配对身份、协议主版本和接管意图，再切换活动连接。当 Bridge 空闲时，配对过的客户端可以自动连接。存在活动客户端时，用户在 Companion 主动连接才允许接管。被接管的客户端暂停自动重连，避免两个实例互相驱逐。

接管请求串行处理。Bridge 先撤销旧会话的处理资格，取消未交付的音频与旧会话的待发命令，再向旧端发送接管原因并关闭连接，随后启用新会话。旧连接不响应关闭时，Bridge 在有限时间后断开。每条命令和每段录音绑定到 Bridge 分配的会话代次，旧回调、晚到的命令和已失效的录音无法修改新会话。旧连接的断开回调只清理自己所属的会话。已经发出的震动脉冲按原时限结束。

接管前已完整接收的录音及其草稿留在原 Companion 的当前工作会话中。新 Companion 从新的手柄状态快照开始，后续只收到新会话的录音和指令结果。连接状态、草稿状态和是否已粘贴分别记录。

## 双向协议

连接使用 TLS 上的 WebSocket。握手协商协议主版本。消息中的请求 ID 用于关联结果，录音 ID 用于接收端去重。以下是首版消息语义。具体字段和二进制音频分帧方式在实现协议 crate 时定稿。

| 方向 | 消息 |
| --- | --- |
| Bridge → Companion | 会话状态、手柄状态、按键动作、录音开始、录音取消、录音元数据和音频数据、命令结果、接管通知 |
| Companion → Bridge | 录音接收确认、录音取消、测试震动请求 |
| 双向 | WebSocket 心跳，连接关闭 |

控制消息用 JSON 文本帧。首版发送已完成的 Ogg Opus，元数据包含录音 ID、来源设备、格式、字节长度和录音时长，后接同一录音的二进制数据。传输设置最大时长、数据大小和队列限制，避免大录音阻塞交接。Companion 收到完整音频后确认，新的录音 ID 在完整接收后自动触发一次转写，识别失败后可以手动重试。断线后清理半包，用户从下一段重试。 [WebSocket 数据帧与心跳](https://www.rfc-editor.org/rfc/rfc6455.html)

## Companion 语音与文字输入

Companion 网络层将已完成的录音交给 ASR 适配层。各引擎接收音频、语言和取消信号，并返回文本或可展示的错误。Ogg Opus 的解码或转换由接收端按引擎要求执行。首版选一个能在当前 Mac 上跑通的引擎，并用中文夹英文技术术语的录音验证效果。商业 API 以后作为显式选择。

录音开始时记录前台应用。录音提示尽量保持原应用焦点，用户编辑草稿时可切回 Companion。确认粘贴前检查原应用是否仍可用，并通过 macOS 适配层恢复焦点及执行粘贴。目标应用无法确认时提供复制。单个草稿只消费一次确认动作，重连和状态同步不能触发粘贴。

## 开发顺序

1. **本机发现和配对。** 用空载 Bridge 验证 Bonjour、配对、WSS 和心跳。
2. **连接接管和恢复。** 验证两个 Companion 的交接、自动重连和过期会话隔离。
3. **手柄适配。** 接入 Stadiacast，显示按键和电量，发送完整 Ogg 录音并完成震动往返。
4. **桌面语音输入。** 接入一个 ASR 引擎，跑通中文草稿编辑、确认粘贴与复制，验证 NSH 与 macOS 权限和焦点的配合。
5. **恢复与边界。** 测试睡眠重连、进程异常退出、重复确认和拒绝局域网权限。

产品文档的首版验收条目用于检查整条链路。先用手柄和当前 Mac 实测，再考虑拆机部署。

## 第一阶段实现记录

2026 年 11 月 20 日，已建立 workspace，并在当前 Mac 上完成真实 mDNS 发现、WSS 配对与连接、双向心跳，以及读取 Keychain 凭据重新连接。Companion 的 NSH 窗口已验证发现列表选择、连接、断开和再次连接。

首次配对使用 Bridge 生成的 pairing.json。文件包含服务端证书和一次性令牌，由当前用户的本机文件路径提供信任来源，有效期十分钟且仅使用一次。Bridge 重启保留身份和长期配对记录，同时刷新邀请。用户选取发现到的 Bridge 后，Companion 校验配对文件里的 Bridge 身份，用文件中的证书建立 TLS 信任，再保存长期凭据。连接状态和每五秒一次的心跳显示在窗口里。

第二阶段已实现单连接接管、自动重连和旧会话失效处理。手动输入网络地址和 ASR 按各自阶段接入。当前 macOS 开发版本通过 scripts/run-companion.sh 构建并打开本地应用。

## 第二阶段实现记录

协议主版本为 2。认证消息包含显式接管标记。Bridge 串行处理通过认证的连接请求，先撤销旧会话的归属并请求关闭，再启用新会话。旧端收到 Replaced 后停止自动重连。自动重连发送不接管请求，Bridge 返回 Busy 时暂停重连并提示用户点击连接接管。心跳携带会话 ID，Bridge 验证当前归属后响应。旧任务只清理属于自身的活动槽位。

Companion 将网络会话作为独立任务管理。用户断开或切换目标时取消并等待旧任务退出，随后再处理新会话。网络异常使用 1、2、4、8 秒上限的退避重试，重试时按 Bridge 身份刷新发现地址。认证拒绝和 Keychain 失败由界面报告。首次配对成功后 Bridge 自动刷新同一文件的短时邀请，使第二个 Companion 可以独立配对。

已通过真实 mDNS 和 WSS 的双客户端回归，覆盖接管、旧端停止重连、错误凭据无权接管、自动请求遇占用、Bridge 重启恢复、旧会话 ID 拒绝和新会话继续响应。当前消息只涉及连接和心跳。手柄接入后的录音取消和旧震动指令隔离将在接入相应功能时验证。

## 第三阶段实现记录

Bridge 使用独立的 Stadiacast 适配模块，启动托管子进程并保存用户提供的网络参数。首版使用状态轮询与结构化日志，避免把控制台摘要作为协议。适配模块核对状态接口的 log_dir 与当前托管目录一致后，才发送麦克风、震动和关停指令。子进程异常退出会报告错误，自动重启留到恢复阶段。

协议主版本为 3，新增硬件状态、按键边沿、录音生命周期、录音元数据、有限时长震动和交付确认。Ogg Opus 完成后按 64 KiB 分块发送，每段带录音 ID、长度、时长与 SHA-256。接收端校验后原子保存到用户数据目录，确认交付，Bridge 随后删除源音频。接收端保留录音供用户打开和后续 ASR 使用。

Bridge 在拥有活动 Companion 时启用 PTT。接管时先撤销旧会话，禁用 PTT 并停止麦克风，排空旧日志并清理旧音频，再绑定新会话。按键仍被按住时，新会话等待松开。过期会话的交付确认和震动请求会被忽略。录音上限为 60 秒和 4 MiB，未确认源音频在 30 秒后清理。

已用当前手柄验证 A、B、X、Y 与 Assistant 事件、短震及 Ogg 音频传输。Companion 收到的一段录音为 3.88 秒、48 kHz、单声道 Opus，完整解码检查通过，源文件在交付后已清理。电量报告到达后，界面已显示 99%。录音路径隔离、取消后晚到音频、失败录音和校验失败已有自动回归。

录音中接管的实物回归也已通过。测试程序在检测到真实音频包后立即接管，确认麦克风关闭、取消的源文件清理、新会话收到的消息不含旧音频或旧录音开始事件。

## 第四阶段实现记录

Companion 接收校验通过的 Ogg 后，由独立转写任务调用本机 Voxt Qwen3-ASR。ASR 适配代码负责音频转换、模型选择和 HTTP 请求，草稿流程负责重试、取消和忽略过期结果。服务地址限于回环 IP，连接绕过代理且拒绝重定向。Bridge 与手柄协议保持版本 3。

草稿默认留在 Companion 窗口，录音和转写期间保持目标应用的焦点。AppKit 记录录音时的应用进程和启动时间，防止向已经重启的同名应用投递。点击粘贴后，如果 Companion 在前台，则尝试切回目标应用。如果另一个应用在前台，提示用户手动切回。自动粘贴投递带目标进程 ID 的 ⌘V，实际插入位置由目标应用的当前光标决定。手柄确认、丢弃仅作用于录音来源的 Bridge 和手柄。连接被接管后保留草稿并允许复制；连接恢复时需要用户再次确认。成功投递后草稿只消费一次。

已用实物手柄验证 5.30 秒录音从 Assistant 松手、经本机 Qwen3-ASR 转写、在 Companion 修正技术术语，到按 A 将修订后的中文混合英文文本插入「文本编辑」。赋予辅助功能权限前，界面提示授权并保留草稿。

下一轮可在现有的「音频和语言输入 → 文本或错误」调用边界引入可切换后端。Voxt 和内置 SGLang Omni 可共用本机 HTTP 客户端，差别在服务进程的所有权。内置模式由 Companion 管理 Apple Silicon 的原生 MLX `qwen3_asr_server`，启动后读取 stdout 的 `ready` 事件以获取动态端口，再使用同样的转写请求。支持鉴权的运行时可从子进程管道接收随机令牌。发布前验证原生运行时、Metal 资源和 Qwen3-ASR 0.6B 4-bit 权重的打包、签名和分发许可，并测首次加载及后续转写延迟。系统识别和商业服务如需不同调用方式，再各自实现后端。

## 第五阶段实现记录

自动恢复使用独立 Restore 命令，不带接管权限。Bonjour 发现保存的 Bridge 后尝试恢复，遇到 Busy 时等待用户主动连接。心跳在睡眠后跳过积累的 tick，超时触发原有重连流程。

Stadiacast 异常退出会取消未完成录音，清理旧进程的音频状态，保留当前 Companion 归属并退避重启。每次启动创建新的日志目录，防止旧日志重新触发录音事件。新增回归验证子进程退出后创建新进程和新目录。真实 WSS 回归已通过 Bridge 重启恢复认证、自动重连不抢占和过期会话隔离。

开发应用使用本机 Apple Development 证书签名。脚本记录未签名构建产物的哈希和签名身份，复用前验证现有签名。发布脚本生成两个 release 产物并验证签名。Companion 保留每次进程启动的诊断日志，Bridge 启动脚本保存控制台输出，Stadiacast 独立保存结构化事件。真实睡眠唤醒、网络切换和跨构建授权保持仍需要实机验收。

2026 年 11 月 20 日，release 包已用用户提供的 Apple Development 身份签名并通过 codesign 验证。更换签名后的 Keychain 读取由用户重新批准。实机睡眠超过 20 秒后唤醒，手柄重新连接成功，Companion 的测试震动有效。Wi-Fi 断开恢复实测中，手柄变为黄灯，Mac 重连后仍需将手柄关机重开。重新开机后按键与震动有效，录音确认后文本只插入一次。因此网络恢复为部分通过，手柄自动恢复尚未通过。
