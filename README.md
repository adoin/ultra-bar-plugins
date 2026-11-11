# Ultra Bar 插件

插件源码仓库，与 `ultra-bar` 放在同一父目录时开发 runner 自动发现。Rune 脚本返回声明式 UI 数据，由宿主绘制；原生面板独立构建，脚本修改无需重新编译宿主。

## 官方插件目录

| 插件 | ID | 版本 |
| --- | --- | --- |
| Schedule Calendar（日程、农历、地区与天气） | `com.emssion.schedule-calendar` | 0.2.0 |
| Everything Search（已安装软件优先搜索） | `com.emssion.everything-search` | 0.2.0 |
| Agent Usage（订阅额度与手动重置余额） | `com.emssion.agent-usage` | 0.2.0 |

在 Ultra Bar“设置 → 插件中心 → 官方插件库”按需安装，或下载 Release 中的 `.ubplugin` 单独安装。0.2.0 插件需要 Ultra Bar 0.2.0 及以上。官方源身份固定为 GitHub repository id `1452116417`，插件身份和更新使用各自反向域名 ID 与 SemVer。

推送 `<插件ID>/v<版本>` tag 触发测试、release 编译、哈希包与目录生成，再发布 GitHub Release；`sdk-revision.txt` 固定 SDK 的完整提交，避免相同 tag 随主分支变化得到不同构建。已发布产物不可覆盖，变更请升版本。

## 最小插件

`examples/counter` 是用于验证点击和热重载的计数器示例（界面文字 `Clicks`），不在默认加载目录中。默认仅加载仓库根目录下带 `plugin.json` 的一级插件目录，因此日常启动不会出现计数器。需要试用时，在主程序目录执行 `pwsh.exe -NoLogo -NoProfile -File ./dev.ps1 -Plugins ../ultra-bar-plugins/examples`。

每个一级子目录是一份开发插件。正式发布使用反向域名 ID，与目录名／原生二进制名分离；以下 counter 是保留的 API v1 本地示例。

```text
counter/
  plugin.json
  main.rn
```

```json
{
  "id": "counter",
  "api_version": 1,
  "entry": "main.rn",
  "width": 120
}
```

```rust
pub fn render(clicks) {
    (format!("Clicks: {}", clicks), "none")
}
```

`render(clicks)` 返回 `(text, action)`：点击次数由宿主持有，成功热重载后保留；重启程序后归零。点击时先执行当前 action，再增加计数、重新计算显示内容。

API v1 的动作白名单：`none`、`start`、`notifications`、`settings`。无任意命令执行接口。`width` 为 96 DPI 下的逻辑宽度，范围 64–320；文本最多 80 个字符。主屏空间不足时只显示能容纳的内容。后续布局/菜单 API 需要显式升级契约。

## API v2：时钟扩展与原生面板

`schedule-calendar` 是第一个正式插件。其 `main.rn` 定义时钟显示、面板动作和日历视图配置；Rust 面板实现日期控件、日程存储与提醒。

```json
{
  "id": "com.emssion.schedule-calendar",
  "version": "0.1.0",
  "name": "Schedule Calendar",
  "api_version": 2,
  "entry": "main.rn",
  "width": 120,
  "slot": "clock",
  "icon": "\ue787",
  "capabilities": ["panel"],
  "panel": { "runtime": ".runtime/current.json", "binary": "schedule-calendar", "background": true }
}
```

- `slot` 为 `taskbar` 或 `clock`，缺省沿用 API v1 的任务栏插件区。时钟槽的 `{time}` 替换为用户设置的时间格式，并按最终文字自适应宽度；`icon` 为一个系统图标字形，空字符串表示不绘制图标。
- `panel` 动作要求 API v2 且声明 `panel` capability。宿主只接受插件目录内的运行指针和 EXE，拒绝越界路径。原生面板属于受信任的本机程序，进程隔离不等于安全沙箱。
- 原生 Cargo 二进制名由 `panel.binary` 声明，与插件 ID 分离。主仓库 `dev.ps1` 会串行编译并写入 `.runtime/current.json`，exe 使用内容哈希目录，避免 Windows 文件占用妨碍更新。运行目录不提交到 Git。
- 面板启动协议：`--plugin-root DIR --owner HWND --parent PID --show`；后台启动使用 `--background`。插件监测父进程并在任务栏退出后结束。`background` 用于本地提醒等明确的后台工作，不需要持续渲染。
- 切换时钟入口只改变入口选择；已安装且声明后台运行的日历仍处理已保存的提醒。

开发时从主仓库执行 `pwsh.exe -NoLogo -NoProfile -File ./dev.ps1`。单独构建原生插件可用 `-Task PluginBuild`，`-Task Check/Test/Clippy` 也会检查原生插件。重新启动新版 runner 后，插件 Rust 源码保存会触发受限构建；Rune 配置仍直接热重载。

## 打 tag 发布

当前官方插件库为本仓库 `adoin/ultra-bar-plugins`，GitHub 仓库 ID `1452116417`。Ultra Bar 从 GitHub Releases 读取 `plugin-catalog.json` 和 `.ubplugin` 资产，按插件 ID 选最新兼容稳定版本，不把源代码目录当成线上插件目录。

1. 修改 `plugin.json.version`；原生插件同步修改 Cargo.toml 的版本，提交 Cargo.lock。
2. 将插件源码和工作流推送到 main；首次发布前，主程序仓库也需要推送 `crates/native-ui` 等 SDK 代码。
3. 在插件仓库创建并推送 `com.emssion.schedule-calendar/v0.1.0` 这样的 tag。
4. `Release plugin` 工作流测试、构建 release 二进制、生成包和 SHA-256、上传目录资产并创建 GitHub Release。带预发布版本的 tag 标记为 prerelease，默认目录不展示。

```powershell
git tag com.emssion.schedule-calendar/v0.1.0
git push origin com.emssion.schedule-calendar/v0.1.0
```

本地正式打包（同样限制 Rust 构建资源）：

```powershell
pwsh.exe -NoLogo -NoProfile -File ./scripts/package-plugin.ps1 -PluginId com.emssion.schedule-calendar
```

输出：`dist/<ID>--<版本>--windows-x86_64.ubplugin`、对应 `.sha256`、`plugin-catalog.json`。调试验证使用 `-DebugBuild`，输出隔离在 `target/package-debug` 或明确指定的测试目录；不用于正式发布。

工作流从主仓库 main 获取 SDK，包内记录实际 SDK 提交和插件源提交。已发布 Release 不覆盖；修复后升级版本并新建 tag。协议和来源切换规则见主仓库 `docs/plugin-ecosystem.md`。

## 热重载

API v3 提供受宿主管理的后台服务，以及 `leading`、`tray` 槽位。现有插件不需要迁移。详见主仓库 `docs/plugin-services.md` 与共享 `crates/plugin-sdk`／`crates/plugin-ui`；新增的 [Everything Search](everything-search/README.md) 复用现有索引，[Agent Usage](agent-usage/README.md) 监测 Codex／Claude Code 订阅额度。对应 tag 为 `com.emssion.everything-search/v0.1.0`、`com.emssion.agent-usage/v0.1.0`。发布前主仓库也需包含这两个 SDK crate。

原生面板使用共享 SDK 的 `ultra_bar_native_ui::theme`：入口调用 `init_from_env()` 读取宿主传入的 `ULTRA_BAR_ACCENT`（`#RRGGBB`），绘制时读取 `palette()`，窗口过程先调用 `handle_message(hwnd, message, wparam)` 处理即时主色更新。`Palette` 字段是 GDI COLORREF，`accent_rgb()` / `Hsl::rgb()` 则为 RGB。激活与选中颜色保持主色 H，任务材质可使用 `theme::relief()` 获取与主色同 H、低饱和度和相对明暗的浮起/凹陷调色；共享左上光源，配合外阴影与内阴影形成深度。背景/字段画刷是 SDK 借用的动态资源，不要缓存到 `WNDCLASS` 或自行销毁；应在擦除与控件绘制消息中获取。宿主不为换色关闭编辑器。

先在主程序仓库运行 `pwsh.exe -NoLogo -NoProfile -File ./dev.ps1`，然后编辑这里的 `.rn` 或 `plugin.json`。文件事件防抖后在后台重载，不重启宿主、不启动 Cargo。语法错误/非法输出保留上一份可用插件，日志见主仓库 `target/dev-watch/app.err.log`。删除插件目录会卸载，恢复目录则重新加载。

最多加载 32 个插件，每个清单/脚本不超过 64 KiB；入口文件必须位于插件目录内部。每次脚本调用有 25,000 条 VM 指令预算及 8 MiB Rune 分配预算，避免死循环阻塞 UI。分配预算不是进程总内存硬上限。当前 API 只支持单文件入口，不开放任意文件导入或系统 API。

