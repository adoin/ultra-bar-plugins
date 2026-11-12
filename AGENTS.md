# 插件开发约定

- API v1 使用 Rune 脚本，无需编译；API v2 可声明受目录限制的独立原生面板。原生面板通过主程序的 `dev.ps1` 串行、限 CPU 构建，禁止在本机无限制并行 Cargo。
- 每个一级子目录对应插件；正式插件使用稳定的反向域名 ID 和 SemVer，目录名可保持短名称。Cargo 二进制名由 `panel.binary` 指定，不能从反向域名 ID 推断。运行副本位于插件自己的 `.runtime/`。
- 日历 ID 固定为 `com.emssion.schedule-calendar`。每个插件以自己的 plugin.json 维护 SemVer，原生 Cargo 版本必须一致。push main 自动对比已成功发布的版本，只构建版本升级的插件；未升级者复用原包。流水线自动生成 `<插件ID>/v<版本>` 包 tag 和 `market/<commit>` 完整目录快照；无需手工打 tag。使用 scripts/package-plugin.ps1 生成 .ubplugin、SHA-256 和单包目录，不覆盖已发布产物。
- 市场目录为 plugin-market.json schema 2，引用同仓库历史 Release 的资产 ID、原下载 URL、SHA-256、包版本和主程序兼容范围；每个包继续附带 schema 1 的 plugin-catalog.json 供旧客户端读取。目录发布失败不得删除已成功发布的插件包；重跑复用现有包。Actions 每个发布工作流保留最近 3 次记录，不清理 Release 包。
- 主程序通过系统文件事件自动重载插件。不要添加扫描轮询或持续动画循环。
- 开发运行入口在同级 `ultra-bar/dev.ps1`；临时实验放 `D:/tmp` 独立任务子目录。
- 产品文案描述功能和操作，不复述开发要求或交付历史。

- 编辑能力统一由主程序的编辑器服务提供。插件只声明类型、数据、选项、约束和业务校验，接收编辑结果；禁止在插件中创建 EDIT、COMBOBOX、日期/时间、颜色、文件、快捷键等编辑控件或调用旧的本地 form::edit。新增编辑类型必须扩展宿主注册表，不允许插件自绘编辑器作为降级。日历、图表、进度等展示渲染仍可由插件自定义。
