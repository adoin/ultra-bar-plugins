# Everything Search

优先搜索“程序和功能”对应的 HKCU/HKLM 卸载注册表（32/64 位视图），排除系统组件和子更新项；开始菜单快捷方式、应用图标或安装目录用于定位启动入口。软件匹配排在文件前，未找到启动入口仍可查看登记信息。不会执行卸载命令。启动时缓存软件列表，手动刷新时更新，后台不扫描安装目录。

齿轮中可关闭默认依赖忽略，或添加用分号分隔的目录名／完整路径。默认忽略 node_modules、.pnpm-store、.venv、site-packages、.git/objects、.cargo/registry、.gradle/caches、.nuget/packages、.yarn/cache；不排除普通 src、build、dist、packages 等目录。规则保存在插件自己的 settings.json，并直接组成 Everything 查询，不修改其全局索引与排除设置。

ID：`com.emssion.everything-search`。搜索入口位于开始按钮旁；点击打开圆角毛玻璃面板。输入后等待 180ms 查询，支持 Everything 搜索语法，最多显示 60 项。方向键／滚轮选择，Enter 或双击打开，Ctrl+Enter 或行右侧图标定位文件，Esc 收起。

复用本机已运行的 Everything，通过官方 Unicode WM_COPYDATA IPC 查询现有数据库。兼容标准实例和 `1.5a` 实例；不读取 Everything 数据库文件，不安装 Windows 服务，不创建索引，不扫描硬盘。Everything 未运行或没有及时响应时在面板显示原因。Windows 服务用于 Everything 自己的 NTFS 权限与监控，不能代替正在运行的 Everything 客户端。

后台进程仅等待宿主和搜索请求，查询有 2 秒超时；不会在面板关闭时轮询索引。文件打开请求交给宿主执行，用户打开的编辑器不属于插件的进程任务组。首次读取可用的结果可能取决于 Everything 索引状态与当前 Windows 权限。

官方协议：[SDK](https://www.voidtools.com/support/everything/sdk/)、[IPC](https://www.voidtools.com/support/everything/sdk/ipc/)、[服务说明](https://www.voidtools.com/support/everything/everything_service/)。

从主项目执行 `pwsh.exe -NoLogo -NoProfile -File ./dev.ps1 -Task PluginBuild`，在插件仓库使用 `scripts/package-plugin.ps1 -PluginId com.emssion.everything-search` 打包。发布 tag：`com.emssion.everything-search/v0.2.0`。
