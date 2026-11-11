# 插件开发约定

- API v1 使用 Rune 脚本，无需编译；API v2 可声明受目录限制的独立原生面板。原生面板通过主程序的 `dev.ps1` 串行、限 CPU 构建，禁止在本机无限制并行 Cargo。
- 每个一级子目录对应插件；正式插件使用稳定的反向域名 ID 和 SemVer，目录名可保持短名称。Cargo 二进制名由 `panel.binary` 指定，不能从反向域名 ID 推断。运行副本位于插件自己的 `.runtime/`。
- 日历 ID 固定为 `com.emssion.schedule-calendar`。发布 tag 为 `<插件ID>/v<版本>`，必须与 plugin.json 和原生 Cargo 版本一致。使用 scripts/package-plugin.ps1 生成 .ubplugin、SHA-256 和发布目录，不覆盖已发布 tag 的产物。
- 主程序通过系统文件事件自动重载插件。不要添加扫描轮询或持续动画循环。
- 开发运行入口在同级 `ultra-bar/dev.ps1`；临时实验放 `D:/tmp` 独立任务子目录。
- 产品文案描述功能和操作，不复述开发要求或交付历史。
