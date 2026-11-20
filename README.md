# Stadia Wave

Use the microphone on your Google Stadia controller to engage in "vibe coding" over Wi-Fi, or to control all your devices.

Wave Bridge 管理手柄连接和网络服务。Wave Companion 在桌面上接收输入，并承载语音识别和文字输入界面。已实现 Bonjour 发现、一次性配对、WSS 连接、Keychain 凭据保存、单连接接管、断线重连和 Stadiacast 手柄接入。

## 在 macOS 上运行

需要 Xcode Command Line Tools。项目通过 `rust-toolchain.toml` 固定 Rust 1.97，rustup 会在首次构建时安装。Companion 使用 NSH 0.6 的纯 Rust 界面。启动脚本构建本地开发 .app，写入 Bonjour 和局域网用途声明，签名后打开窗口。也可以用 cargo run -p wave-companion 启动开发窗口。

在项目根目录打开两个终端，分别运行。

```sh
cargo run -p wave-bridge
```

```sh
./scripts/run-companion.sh
```

Bridge 默认监听 TCP 8787 并发布 `_stadia-wave._tcp.local.`。如 macOS 提示局域网访问，允许这两个程序使用局域网。在 Companion 中选中发现的 Bridge，将 Bridge 终端显示的 `pairing.json` 绝对路径粘贴进配对文件输入框，点击「连接」。

配对文件有效期为十分钟且只可使用一次。配对成功后，长期凭据保存在 macOS Keychain。点击「断开」后再次「连接」即可用已保存的凭据重连。每次配对成功后，Bridge 自动更新同一路径的邀请，可继续配对另一个 Companion。Bridge 重启保留身份和配对记录，同时生成新的短时邀请。

Bridge 配置和身份默认保存在用户的 Application Support 目录。可以显式指定。

```sh
cargo run -p wave-bridge -- --name "书房 Bridge" --port 8787 --data-dir /absolute/private/wave-bridge
```

两个程序同机时也通过 mDNS 发现的网络地址连接。点击「连接」会主动接管 Bridge 的现有连接。旧端显示被接管并停止自动重连。短暂断网后，Companion 按 1、2、4、8 秒间隔重试。如果 Bridge 已被另一端占用，停止重试并提示用户主动接管。点击「断开」可取消重连。手动填写网络地址按后续阶段实现。

## 验证

```sh
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Bridge 启动后，使用以下诊断程序验证真实 mDNS 发现、首次配对、双向心跳以及从 Keychain 读取凭据重连。它复用 Companion 的网络实现。验证结束后会断开连接。

```sh
cargo run -p wave-companion --example network_check -- /absolute/path/to/pairing.json
```

完整产品流程见[产品文档](docs/product.md)，模块与协议设计见[技术方案](docs/architecture.md)。

## 验证单连接接管

以下回归程序启动一个独立 Bridge，使用两个独立的 Companion 身份测试首次配对、接管、旧端停止重连、错误凭据拒绝、自动重连遇到占用、Bridge 重启恢复与过期会话指令。测试结束会清理自己的子进程、配对记录和临时文件。

```sh
cargo build --workspace --bins --examples
cargo run -p wave-companion --example connection_check -- ./target/debug/wave-bridge
```

手动测试两个桌面实例时，可以为开发进程设置不同的凭据 profile。各 profile 单独配对和保存 Keychain 凭据。两个普通实例也能通过相同身份主动接管。

```sh
WAVE_COMPANION_PROFILE=second cargo run -p wave-companion
```

## 接入 Stadiacast

准备 Stadiacast 1.3.0 可执行文件，使用已经验证成功的网络参数启动 Bridge。下面地址是当前开发机器的配置，部署时替换为实际地址。手柄的 DHCP DNS 配置由用户在路由器维护。

```sh
cargo run -p wave-bridge -- \
  --stadiacast /absolute/path/to/stadiacast \
  --controller-ip 192.168.0.191 \
  --advertise-ip 192.168.0.166 \
  --dns-upstream 192.168.0.1
```

Bridge 将配置保存在其数据目录的 stadiacast.json。之后直接启动 Bridge 即可复用。可执行文件路径需要长期有效。当前开发机器已将二进制复制到 Bridge 数据目录的 runtime/stadiacast，以便独立于参考项目运行。

Bridge 启动独立 Stadiacast 子进程，启用 DNS、录音和震动会话。每次运行的日志写入数据目录 captures 下的独立子目录。退出 Bridge 时会停止托管的 Stadiacast。启动失败、端口冲突和进程退出原因会显示在 Companion。

在 Companion 连接后，手柄面板显示在线状态和已报告的电量。点击「测试震动」发送 300 毫秒短震。按住 Assistant 录音，松开后音频通过 WSS 传输，接收端校验 SHA-256 并保存 Ogg Opus。「打开录音」交给系统默认播放器。接收文件在 Companion 数据目录的 recordings 下保留，Bridge 在收到交付确认后清理源文件。

首版每段录音上限 60 秒、4 MiB。接管或断开时停止旧录音，丢弃未完成内容。新连接等待 Assistant 松开后接受下一次按下。状态每 500 毫秒读取 Stadiacast 的状态 API，按键边沿和录音结束来自结构化日志。

对录音中接管做实物验证时，先连接 Companion，再运行以下诊断程序。看到 READY 后按住 Assistant，程序检测到真实音频包后立即接管，检查麦克风关闭、旧文件清理和新会话没有收到旧音频。程序需要当前机器上已经配对的默认身份。

```sh
cargo run -p wave-companion --example recording_takeover -- /absolute/path/to/pairing.json
```

## 使用手柄语音输入

1. 在 Companion 连接 Bridge，确认手柄已就绪。让 Voxt 的本机 Qwen3-ASR 服务运行，安装可供 Companion 调用的 FFmpeg。
2. 在「本机语音识别」填入服务地址，例如 `http://127.0.0.1:58078`。Voxt 重启后端口可能改变，可用 `lsof -nP -iTCP -sTCP:LISTEN | rg qwen3_asr` 查看。识别语言默认 `zh`，留空由模型判断。地址和语言会记在 Companion 本地。
3. 在目标程序中放好光标，按住手柄 Assistant，等指示灯闪后说话并松手。切回 Companion 可检查和编辑草稿。按手柄 A 或点「粘贴」确认，按 B 或点「丢弃」清除草稿。
4. 首次自动粘贴时，在 macOS「系统设置 → 隐私与安全性 → 辅助功能」中启用 Wave Companion。也可点「复制」，自行回到目标程序粘贴。自动粘贴同样会把草稿写入系统剪贴板。开发 .app 重新签名后，可能需要重新授权。

本机 ASR 使用 `GET /v1/models` 选择唯一模型，并通过 `POST /v1/audio/transcriptions` 发送转成单声道、16 kHz PCM WAV 的录音。可用 `WAVE_ASR_URL`、`WAVE_ASR_LANGUAGE` 和 `WAVE_ASR_MODEL` 覆盖设置。若 FFmpeg 不在 Homebrew 常用目录或 `PATH` 中，可设置 `WAVE_FFMPEG` 为绝对路径。

服务不可用时，修改地址后点「重试转写」即可使用原录音。Companion 同时保留一份待处理草稿。在它处理完之前收到的新录音仍会保存，可通过「打开录音」播放。
