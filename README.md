# 知微 / Zhiwei

本地优先的对话式记忆/知识助手。工程名仍为 `smart-assistant`；品牌名是「知微」——取“见微知著”之意，它从细节中理解并记住你。

核心是一个 Rust 内核（`backend/`），web 与 Android 只是它的两种消费形态：

- **backend/**：Rust 内核（对话工具循环 + 长期记忆 + 语义检索 + 主动开口），可选的桌面调试 API（Axum）
- **frontend/**：Web 端（Vite + React），直接消费 HTTP / SSE 接口
- **android/**：Android 端（占位；UniFFI 导出暂已移除，待内核与对话稳定后再接）

## 快速启动（根目录）

```bash
make dev      # 一键启动：自动生成证书（缺失时）+ 后台后端 + 前台前端
make stop     # 停止占用 3000/5173 端口的进程
make build    # 编译后端 + 构建前端
make test     # 后端测试
```

`scripts/dev.ps1` 与 `Makefile` 等价（无 make 时直接 `powershell -File scripts/dev.ps1`）。后端日志写入 `.dev-server.log`；退出 `npm run dev` 时自动停止本次启动的后端。

## 桌面程序（Windows）

`desktop/` 是知微的 Tauri 2 托盘壳，管理独立的 `zhiwei-service.exe`。当前 P1 已支持：

- 默认仅驻留右下角托盘，不打开主窗口；
- 左键托盘打开状态窗口，右键菜单可启动、停止、重启、查看日志或退出；
- 关闭设置窗口只隐藏，不中断通道服务；
- 同一可执行文件只允许一个实例，重复启动会唤起已有窗口；
- 开发版与正式版按可执行文件路径区分，不共享单实例锁和 WebView2 数据目录。

开发运行：

```powershell
cd desktop
npm install
npm run desktop:dev
```

构建便携正式版，不生成 MSI/NSIS：

```powershell
pwsh -File scripts/build-portable.ps1
```

默认输出到新目录 `D:\Zhiwei`，入口是 `D:\Zhiwei\zhiwei-desktop.exe`。默认只构建并
打包运行必需的 `zhiwei-service.exe` 与桌面壳；维护工具不参与日常发布构建。
首次创建时会从旧目录 `D:\SmartAssistant` 迁移 `.env`、数据库和本地状态；
日志从新目录重新开始，避免复制历史敏感信息。之后只读写新目录。脚本会更新桌面的
`Zhiwei.lnk` 指向新入口；不需要时加 `-NoShortcut`。旧目录不会被删除。
启动配置与密钥仍写在 `.env`；运行时策略由桌面通过 `zhiwei-service` 本地控制接口
写入数据库。需要维护工具时单独运行：

```powershell
pwsh -File scripts/build-tools.ps1 -InstallDir D:\Zhiwei
```

它会按需构建单一运维工具 `zhiwei-cli.exe`，不会重新构建桌面程序或服务主程序。

`zhiwei-cli` 不提供对话入口，只负责运维：

```powershell
zhiwei-cli db check
zhiwei-cli db rebuild --force
zhiwei-cli sticker fetch <贴纸包名>
zhiwei-cli sticker show
zhiwei-cli config get
zhiwei-cli config schema
zhiwei-cli config set <key> <value>
zhiwei-cli config audit
```
旧 PowerShell 托盘脚本仅作为过渡方案保留。

开发约定见 [`AGENTS.md`](AGENTS.md)。
