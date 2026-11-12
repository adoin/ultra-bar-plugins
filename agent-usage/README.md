# Agent Usage

## 统计面板

点击胶囊打开双层 tabs 面板：顶层选择已检测到的 Codex、Claude Code、Grok Bot，第二层为概览、项目、最近几天、Token 构成、模型。支持最近 7／14／30 天，长列表使用滚轮。原额度／连接错误列表已由统计面板替代；未运行的已检测客户端仍保留入口。

统计沿用 Codex Weather Widget 的累计计数差值口径，补充输入、缓存输入、输出、推理输出；缓存输入属于输入、推理输出属于输出，比例不重复相加。会话 ID 去重，累计计数回落按新计数段处理，排除子 Agent 的继承前缀；Claude 按 message ID 合并流式 usage，防止重复计数。仅提取时间、项目、模型和用量元数据，不展示、缓存或发送聊天正文。

首次后台索引后按文件偏移增量读取，索引与统计快照保存在插件数据目录；仅面板打开且有活动记录变更时触发增量整理，关闭后不持续扫描。资源仍受宿主 CPU／内存 Job 限制。胶囊重置次数独立右对齐，短后缀不省略；文字使用柔和暗色投影与近身描边两层静态阴影，由 GPU 与气泡一起合成。

Grok Bot 读取 `%APPDATA%/Grok Bot/sand-client-persistence` 中唯一的 `weekly-usage.cache` 快照，支持 Premium 等计划的剩余百分比，即使未运行也可查看。最多采用24小时内且未跨重置周期的快照，不读取其 secrets／Cookie／令牌文件，不代登录或后台唤醒该客户端。当前本地缓存没有项目、逐日 Token 或 Token 构成明细，这些维度明确显示未提供，不由额度百分比推算 Token。

进度填充区使用提高饱和度和亮度的主色半透明底，气泡本身为白色。整个胶囊使用左上光源的内凹柔和阴影；未填充区域的中心保持零 Alpha，透明区域仍保留鼠标命中与提示。

任务栏进度采用 Sax Design progress 的 bubbles 微气泡纹理，填充层和轨道使用胶囊圆角，颜色跟随宿主主色。气泡通过硬件 DirectComposition 时间轴每 2 秒循环上浮，纹理只在额度、主色、尺寸或内容变化时上传，不使用 CPU 帧计时器重绘任务栏；0%、100% 和未知额度不播放气泡动画。多 Agent 的短时纵向切换继续保留。硬件合成不可用时回退静态材质。

ID：`com.emssion.agent-usage`。在托盘左侧显示活跃 Agent 的订阅剩余额度。当前实现 Codex、Claude Code 与 Grok Bot：结合前台应用／标题和客户端会话文件的变更事件判断活动，不保留聊天正文。前台 Agent 持续显示，后台活动保留 5 分钟；多个 Agent 每 5 秒纵向切换，动画只运行 180ms。没有活跃 Agent 时不占位置。点击查看各额度窗口；多个窗口取最小剩余值作为条形值，悬停可查看各项详情。

正常每分钟刷新一次，临时错误按 15、30、60 秒逐步退避，最多等待 5 分钟；手动刷新可重试。最近15分钟内的成功结果可用于短暂超时和正常重启，提示中标明刷新状态和上次读取时间；缓存按 CLI home 和认证文件元数据隔离，不保存凭据，认证错误或缓存过期不继续显示旧值。无数据、API key 账户没有订阅额度或登录失效时显示 `—`，不会把花费、上下文长度或估算 token 数当作订阅剩余额度。

## Codex

识别 `Codex.exe` 及 Windows `OpenAI.Codex_*` 包内的 `app/ChatGPT.exe`；不会把普通 ChatGPT 客户端判作 Codex。桌面版历史库的文件修改事件也作为活动信号，仅检查元数据，不读取数据库或聊天内容；启动时恢复最近5分钟的活动，额度查询自身引起的文件事件不用于延长活跃状态。首次连接允许60秒初始化，后续账户请求最多等待30秒，保持宿主既有 CPU 与内存限制。

使用独立的本地 `codex app-server`，复用 CLI 已有登录；只调用 `initialize`、`initialized`、`account/rateLimits/read`，不创建线程或模型请求，也不消费重置额度。查询返回的 `usedPercent` 转为剩余百分比，支持主／次窗口和窗口重置时间。登录、认证刷新由 Codex 处理，不复制其凭据。

当响应包含 `rateLimitResetCredits.availableCount`，胶囊显示 `↺次数`，详情列出“手动重置剩余 N 次”。缺失字段保持未知，不推断为零，不根据明细数组长度推算余额。[官方 app-server 协议](https://learn.chatgpt.com/docs/app-server#6-rate-limits-chatgpt)。

优先发现 Codex 桌面客户端附带的 CLI，再查 PATH 中的 `codex.exe`；可通过 `ULTRA_BAR_CODEX_EXE` 指定绝对路径。首次连接将 `CODEX_HOME` 目录保存为插件数据中的 `connections.json`，供以后从 Explorer 启动时复用；也可修改该文件的 `codex_home`，不在其中保存令牌。

[官方 app-server 文档](https://learn.chatgpt.com/docs/app-server)。

## Claude Code

优先读取官方 statusLine 的 `rate_limits` 数据，接受 `five_hour.used_percentage` 与 `seven_day.used_percentage`。仅订阅账户等受支持账户在第一次 API 响应之后提供该字段；缺失时显示未知。

插件附带一个可组合的 feed 入口：

```powershell
agent-usage.exe --claude-statusline
```

把 Claude 的 statusLine JSON 作为 stdin 传入即可。程序只保存 `rate_limits`，丢弃其余输入；默认写入 `%LOCALAPPDATA%/UltraBar/plugins/com.emssion.agent-usage/claude-rate-limits.json`，也可在参数末尾指定数据目录。已有 statusLine 可以在自己的脚本中把同一份 JSON 传给此入口，保留原有输出；插件不会自动改写 Claude 配置。请引用已安装插件的有效 EXE 路径，更新安装后路径可能变化。5 分钟未更新的快照不作为实时数据。

没有新鲜 feed 时，兼容读取 `CLAUDE_CONFIG_DIR` 或 `%USERPROFILE%/.claude/.credentials.json` 的既有订阅登录，并向固定 HTTPS 地址 `https://api.anthropic.com/api/oauth/usage` 查询。此兼容接口不是 Claude 承诺稳定的公开 API，可能变化；不进行 OAuth 刷新、不修改认证文件、不跟随重定向、不记录令牌。登录过期请通过 Claude CLI 处理。

[官方 statusLine 字段说明](https://code.claude.com/docs/en/statusline)、[官方认证说明](https://code.claude.com/docs/en/authentication)。

从主项目执行 `dev.ps1 -Task PluginBuild`。打包使用 `scripts/package-plugin.ps1 -PluginId com.emssion.agent-usage`；发布 tag：`com.emssion.agent-usage/v0.2.0`。

统计窗口没有独立标题栏；Agent 导航与紧凑工具放在顶部，指标使用统一轻量信息带，日用量采用细柱、数值标签和淡网格。项目／模型与 Token 明细为轻量列表。点击同一任务栏插件区域切换展开／收起，点击其他程序自动隐藏；外部关闭后同一次任务栏点击不会立刻重开。
