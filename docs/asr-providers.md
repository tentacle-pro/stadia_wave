# ASR 服务接入

## 目标

ASR 不该绑定某一家服务。用户可能用本机模型（Voxt / mlx-audio）、OpenAI 兼容网关、云厂商
原生 API，或公司内网的服务。设计目标：

- 同一套界面支持多种接入方式，由用户自带 key / 账号（BYOK / BYOA）。
- 换服务不改界面代码，只加一个连接器。
- 失败时说清楚是哪一层（地址、鉴权、模型、音频格式）出错。

## 现状

已有两个连接器，请求形状集中在 `Connector` 里，而不是散在界面代码：

| id | 界面名称 | 地址 | 凭据 | 端口发现 |
| --- | --- | --- | --- | --- |
| `local-openai` | 本机 OpenAI 兼容服务 | 必须是 loopback | 无 | 有 |
| `openai-compatible` | OpenAI 兼容服务 | http/https 根地址（可含 `/v1`） | API key，存 Keychain | 无 |

设置页顶部可以切换 Provider，表单按 `Provider` 元数据渲染；`asr::check` / `asr::transcribe`
接收一个 `Target { provider, endpoint, api_key }`，由 `connector_for()` 选择实现。

两者共用同一种 HTTP 形状：

```
GET  /v1/models
POST /v1/audio/transcriptions     multipart: model, language, file(wav)
```

- `Provider` 元数据（`asr::PROVIDERS`）描述界面要什么：地址标签、占位符、是否显示语言、
  是否支持「检测」、是否需要 API key。
- 地址校验按 Provider 分：`local-openai` 要求 loopback 且路径为根；`openai-compatible` 接受
  http/https 根地址，允许 `/v1` 后缀。拼接口时只保留一个 `/v1` 段。
- 失败按状态码给出可行动的说明：401/403 指向 API key，404 指向地址路径，其他情况带响应片段。
- 密钥写在 `dev.stadiawave.companion.asr` 服务下的 Keychain 条目里，按 Provider id 区分；
  空值会删除条目。环境变量 `WAVE_ASR_API_KEY` 可覆盖。
- Voxt 的端口动态分配，启动时用 `pgrep qwen3_asr_server` + `lsof -nP -a -p PID -iTCP -sTCP:LISTEN`
  读出监听端口（「检测」按钮同一条路径）。这只识别 Voxt 的进程名，换本机服务需要新连接器或手填地址。

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
2. ~~加第二个连接器（OpenAI 兼容），验证抽象够用。~~（已完成）
3. 声明式插件 + 沙箱 + 测试流程。
4. 聊天式生成连接器。

## 这一阶段到此为止

两个连接器已经覆盖本机服务与远程 OpenAI 兼容服务，抽象够用。以下留到真正需要时再做：

- **每个 Provider 自己的字段。** 现在元数据是几个布尔开关；要支持 Azure 的 `api-key` 头加
  deployment 路径、Google 的 project/location、实时流式的采样率，需要把 `Provider` 改成字段列表
  （`address` / `secret` / `select` / `text`），由界面按字段渲染。触发条件：出现第一个无法用现有
  字段表达的服务。
- **能力矩阵。** `streaming`、`diarization`、热词、翻译等能力，用于按 Provider 隐藏不支持的选项。
- **错误分类。** 把 `failure()` 的字符串细化成 `auth` / `endpoint` / `model` / `audio` 分类，
  界面据此决定是引导改密钥、改地址还是改录音。
- **声明式插件。** 请求形状、鉴权、字段映射写成清单，在沙箱里执行；生成后必须通过 `probe()` 与
  一次真实转写，再让用户确认。先做 1 和 2，再考虑清单化。
- **聊天式生成连接器。** 最后一步，依赖前面三项成熟；生成结果仍要过测试与用户确认。

在此之前，新增服务优先按现有 `Kind` 加一个分支，保持界面与协议不变。
