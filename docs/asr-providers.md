# ASR 服务接入

## 目标

ASR 不该绑定某一家服务。用户可能用本机模型（Voxt / mlx-audio）、OpenAI 兼容网关、云厂商
原生 API，或公司内网的服务。设计目标：

- 同一套界面支持多种接入方式，由用户自带 key / 账号（BYOK / BYOA）。
- 换服务不改界面代码，只加一个连接器。
- 失败时说清楚是哪一层（地址、鉴权、模型、音频格式）出错。

## 现状

第一个连接器 `local-openai` 已经存在，请求形状集中在 `Connector` 里，而不是散在界面代码：

```
GET  /v1/models
POST /v1/audio/transcriptions     multipart: model, language, file(wav)
```

- `Provider` 元数据（`asr::PROVIDERS`）描述界面要什么：地址标签、占位符、是否显示语言、
  是否支持「检测」。
- 设置页只读元数据来渲染表单；`asr::check` / `asr::transcribe` 是默认连接器的薄封装。
- 地址必须是 loopback（`local_endpoint`），这是 `local-openai` 这一种连接器的约束。Voxt 的端口
  动态分配，启动时用 `pgrep qwen3_asr_server` + `lsof -nP -a -p PID -iTCP -sTCP:LISTEN` 读出
  监听端口（「检测」按钮同一条路径）。这只识别 Voxt 的进程名，换本机服务需要新连接器或手填地址。

## 连接器模型

把「怎么连」和「界面」分开：

| 层 | 职责 |
| --- | --- |
| `Provider`（数据） | `id`、显示名、能力（语言、流式、热词）、需要的字段（地址 / 密钥 / 模型）、默认值 |
| `Connector`（行为） | `probe()`、`models()`、`transcribe(audio, opts)`，以及把自己的错误映射成统一错误 |
| 设置页 | 列出 Provider，按字段生成表单，调用 `probe()` 测试，保存到 Keychain / 配置文件 |

内置建议：

1. **local-openai**：本机 OpenAI 形状的服务（Voxt 等），可自动发现端口。
2. **openai-compatible**：base URL + API key，覆盖大多数自建网关。
3. **云厂商原生**：需要签名、长连接或私有编解码的单独实现（例如实时流式接口）。

凭据进 Keychain，非敏感配置进 Companion 数据目录；`WAVE_ASR_URL`、`WAVE_ASR_LANGUAGE`、
`WAVE_ASR_MODEL` 仍可覆盖，方便开发和 CI。

## 插件方向

如果连接器数量增长到内置维护不过来，可以让用户描述需求、由内置 ChatBot 生成**声明式连接器**：
请求方法 / 路径、鉴权方式、字段映射、响应取值路径。声明式清单在沙箱里执行，不直接跑任意代码；
生成后必须经过 `probe()` 和一次真实转写，再由用户确认。清单可导出、审阅、签名分享。

这条路的成本在审核与错误处理，不在生成；先做 1 和 2，再看是否值得。

## 分阶段

1. ~~把现有 OpenAI 形状抽成第一个连接器，保留本机端口发现。~~（已完成）
2. 加第二个连接器（OpenAI 官方或 Azure），验证抽象够用。
3. 声明式插件 + 沙箱 + 测试流程。
4. 聊天式生成连接器。
