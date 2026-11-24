# macOS 权限归属

Wave Companion 自己不做网络访问：它启动 `wave_bridge`，后者再启动 `stadiacast`。但 macOS 的隐私授权
记录的是**责任进程**（responsible process），而不是真正发起连接的那个子进程。这份文档说明各种启动方式下
权限落到谁头上、界面上怎么显示、以及怎么判断。

## 权限落到哪个 App

| 实际启动方式 | 授权对象（TCC 里的条目） | 概览页「启动方」显示 |
| --- | --- | --- |
| 访达 / 登录项 / `open` 打开 Wave Companion.app | Wave Companion.app | 系统（访达或登录项） |
| Companion 里点「启动」本机 Bridge | Wave Companion.app | 同上 |
| 单独打开 Wave Bridge.app | Wave Bridge.app | —（在 Bridge 自己的进程里） |
| 从 Zed 或 Terminal 里 `cargo run` / 直接跑二进制 | 该终端 App（Zed / Terminal） | Zed（经终端）等 |

要点：

- **不是 `stadiacast`。** 它是隔了两层的子进程；换 stadiacast 版本不会让权限失效。
- **入站不受本地网络权限限制。** 手柄连 `:443` 能成功，只有接收端主动发往手柄的 STUN/DTLS 会被拦。
  所以「注册成功、`display_state` 停在 `dtls-connected` 之前、`stun_in=0`」是权限缺失的典型特征，
  而不是 DNS/API 没拦截。
- **AP 隔离**（路由器把无线客户端互相隔离）会产生一模一样的症状，排查时先排除它。

## 开发构建与安装版

- TCC 条目按**代码签名 + 路径**区分。同一 bundle id 放在 `target/` 下和放在 `/Applications` 下会是
  **两条独立记录**，列表里名字相同，要靠条目右侧「选项 → 在访达中显示」区分。
- ad-hoc 签名的开发构建每次重编译 CDHash 都会变，系统可能把它当成新 App **重新询问**。想减少这种反复：
  给开发构建使用固定的 Developer ID 签名，并固定输出路径。（与 `docs/product.md` 的签名约束一致。）
- 这些只写在文档里，**界面不区分开发版**；界面只回答「我现在是谁、谁启动的我」。

## 界面显示什么

概览页右栏（`apps/companion/src/pages.rs`）显示三行，值来自 `apps/companion/src/desktop.rs::identity()`：

| 行 | 内容 | 来源 |
| --- | --- | --- |
| 运行身份 | bundle 名（不在 bundle 里时用 `Wave Companion`） | 从 `current_exe()` 向上找 `.app` |
| 位置 | bundle 路径（超长时保留尾部），非 bundle 时为可执行文件路径 | 同上 |
| 启动方 | 最近一个祖先 App 的名字；中间隔着 shell 时加「（经终端）」 | `proc_pidinfo` 逐级 `pbi_ppid` |

下面的「系统权限」列出**授权对象就是上面「运行身份」那个 App**：

- **设备控制和数据访问**（macOS 27 之前的「辅助功能」）：可读状态（`AXIsProcessTrusted()`），用于确认粘贴。
- **本地网络**：macOS 没有公开 API 可读授权状态；常态显示「首次连接手柄时询问」，Bridge 上报
  `HardwareState.network_blocked` 时显示「可能未授权（收不到手柄输入）」。
- **驱动扩展**：只在检测到 OpenJoystickDriver 时出现（`systemextensionsctl list` 里有它的扩展，
  或 `/Applications/OpenJoystickDriver.app` 存在），状态为「已启用」/「等待启用」。

下面那行提示会跟着当前选中的 Bridge 变：连的是本机 Bridge 时写「授权对象是上方「运行身份」」，
连的是别的电脑上的 Bridge 时写「权限由 Bridge 所在电脑授予」。

## 本地网络：无法读状态时怎么判断

Bridge 读取 `server.jsonl`：

- 出现 `rtc_socket_error`，且 `error` 含 `EHOSTUNREACH` 或 `No route to host` 时置位
  `HardwareState.network_blocked`；
- `rtc_pair_verified`、`dtls_connected`、`display_state` 为 `ready`/`input`，或任一控制器 `ready`
  时清零；
- 快照只在有控制器在线（`controllers` 非空）时上报该标志，避免手柄关机时误报。

Companion 直接读这个字段，让「本地网络」一行在「首次连接手柄时询问」与「可能未授权（收不到手柄输入）」
之间切换。需要更细的证据时，仍可交叉验证 `rtc_stats.stun_in == 0 && stun_out > 0`。

## 打开系统设置的深链

| 目标 | 深链 |
| --- | --- |
| 本地网络 | `x-apple.systempreferences:com.apple.preference.security?privacy-localnetwork` |
| 设备控制和数据访问 | `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility` |
| 登录项与扩展（含驱动扩展） | `x-apple.systempreferences:com.apple.LoginItems-Settings.extension` |

新版扩展 ID（`com.apple.settings.PrivacySecurity.extension`）不接受 `?privacy-accessibility` /
`?privacy-localnetwork` 作为锚点：用这两个 URL 时系统只是切到「隐私与安全性」，日志里没有 `anchor:`。
legacy 形式会被转发到同一个扩展并正确定位（`System Settings` 日志显示
`anchor: Privacy_Accessibility` / `anchor: Privacy_LocalNetwork`），该扩展的 Info.plist 也声明了
`legacyBundleIdentifier = com.apple.preference.security`。所以代码里用 legacy 形式加首字母大写的锚点。

macOS 27 把「辅助功能」改名为「设备控制和数据访问」；界面行文案用系统的新名称。

带 `revealElementKeyName` 的服务（如 `Privacy_Accessibility`）用旧的键名当锚点。
用错形式时系统只会切到「隐私与安全性」的顶层，不报错，所以两种形式都要实测。

**本地网络是个例外：这一版 macOS 没有可用的锚点。** 实测 17 个候选
（`Privacy_LocalNetwork`、`privacy-localnetwork`、`PrivacyLocalNetworkService`、
`Privacy_LocalNetworkServiceView` 等）、两种扩展 host、以及“先打开面板再带锚点”的两步打开，
都只停到「隐私与安全性」顶层；作为对照，`Privacy_Accessibility`、`Privacy_Microphone`、
`Privacy_Camera` 都能进入各自子页（用系统日志里 `navigationStackDidChange` 的次数判定：
进子页 8 条，只到顶层 4 条）。「本地网络」面板在 `TCCServiceList.plist` 里没有
`revealElementKeyName`，扩展里也没有对应的 `Privacy_*` 字面量。

因此「本地网络」这一行保留文档化的锚点（将来系统支持时自然生效），并在行下提示
「打开后在「隐私与安全性」列表里选「本地网络」」。

## 将来启用虚拟手柄时（`--gamepad ojd`）

这一项**不属于 Companion 自己的权限**，而是第三方驱动扩展：

1. 安装并运行 OpenJoystickDriver.app；
2. 「系统设置 → 通用 → 登录项与扩展 → 驱动扩展」中打开 `OpenJoystickDriver.app`（扩展宿主是
   `XboxUSBDevice`）；
3. 按需在「隐私与安全性 → 设备控制和数据访问 / 输入监控」里允许 OpenJoystickDriver.app；
4. 确认扩展真正激活，而不是停在等待用户：

   ```sh
   systemextensionsctl list | grep -i openjoystick
   # [activated enabled]      → 可用
   # [activated waiting for user] → 还差第 2 步的开关，或需要重启
   ```

5. stadiacast 侧只需要 `--gamepad ojd`；虚拟手柄建不起来时日志是
   `gamepad_failed ... E2005: The service did not reply within 0.5s`，说明 OJD 的服务没响应。

Companion 的「系统权限」在检测到 OJD 时列出「驱动扩展」一行：状态来自 `systemextensionsctl list`
（`[activated enabled]` → 已启用，其余 → 等待启用），按钮打开「登录项与扩展」。这一行不依赖 Bridge
的启动参数，因此 OJD 已安装但当前没用 `--gamepad ojd` 时也会出现。
