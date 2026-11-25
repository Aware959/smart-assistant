# Zhiwei App

`zhiwei-app` 是知微的应用壳（Windows 托盘与本地服务托管；未来的 Android APK 也走
这份代码）。它管理独立的 `zhiwei-service.exe`（通道守护），
不把 Telegram、Agent 或数据库逻辑搬进应用进程。通道的运行时配置
通过 `zhiwei-service` 的 loopback 控制 API 读写，应用进程不直连 SQLite。
界面使用 React + Ant Design 5（暗色主题），代码按页面拆分在 `src/pages/`。

## 开发

一键编排（编译后端调试构建，脚本先拉起 service，再启动
`tauri dev`；应用壳在开发态只当 UI，不自启这两个进程）：

```powershell
pwsh -NoProfile -File scripts/dev.ps1     # 仓库根目录也可 make dev
pwsh -NoProfile -File scripts/dev.ps1 -Telegram
```

`scripts/dev.ps1` 的参数：

| 参数 | 默认 | 说明 |
| --- | --- | --- |
| `-Telegram` | 关 | 打开 Telegram 通道；默认 `TELEGRAM_ENABLED=0`，避免开发态误发消息 |
| `-WebPort <端口>` | `3002` | 开发 service 监听端口；与正式版（3000）、测试版（3001）分开 |
| `-GatewayPort <端口>` | `8788` | 开发网关端口；与正式版（通常 8787）分开，避免抢占 |
| `-LogLevel <级别>` | `info` | 传给服务进程的 `RUST_LOG`；本机若全局是 `warn`，开发态就看不到启动日志 |
| `-SkipBuild` | 关 | 跳过 `cargo build --bin zhiwei-service` |

开发态读自己的 `backend/.env.dev` 与 `backend/smart_assistant.db`，与正式版
（`D:\Zhiwei`，`.env.prod`）不共享库、端口或 bot。

手动方式照旧。在存在 `backend/.env.dev` 和已编译的 `backend/target/debug/zhiwei-service.exe` 时：

```powershell
# 终端 1：后端
cd backend
cargo run --bin zhiwei-service

# 终端 3：应用 UI
cd app
npm install
npm run app:dev
```

`tauri dev` **不会**自动启动 service。应用壳通过 `logs/control.json`
里的 pid 和网关的 `/health` 识别外部进程，界面显示"已在运行（外部启动）"，
不会重复拉起；这类进程也不归应用壳停止 / 重启，请在启动它的终端里操作。
发布版（release 构建）仍由应用壳托管自启。

开发时可用环境变量指定服务程序和工作目录：

```powershell
$env:ZHIWEI_SERVICE = "D:\MyPoject\Rust\smart-assistant\backend\target\release\zhiwei-service.exe"
$env:ZHIWEI_HOME = "D:\MyPoject\Rust\smart-assistant\backend"
npm run tauri dev
```

不设置时，应用壳优先寻找可执行文件同目录的 `zhiwei-service.exe`；开发构建会向上查找
仓库里的 `backend/target/release/zhiwei-service.exe` 或 `backend/target/debug/zhiwei-service.exe`。
排查设置窗口时可加 `$env:ZHIWEI_SHOW_ON_START = "1"`，让窗口在启动时显示。

只看界面、不启动任何后端进程时，可生成并打开 mock 预览页：

```powershell
npm run preview          # 由 index.html 派生 preview.html 并注入 mock 数据
npm run dev              # 打开 http://localhost:1420/preview.html
```

## 当前能力

- 默认只显示托盘，不打开主窗口；
- 左键托盘图标打开状态窗口，右键菜单提供启动、停止、重启、日志、目录和退出；
- 自动启动 `zhiwei-service.exe`，异常退出后按退避间隔重启；
- 单实例运行，重复启动会唤起已有窗口；
- 关闭状态窗口只隐藏，托盘继续运行；
- 构建配置关闭安装器打包，输出仍是可直接运行的便携程序。

状态窗口同时展示运行时配置，保存时使用 revision 乐观锁。配置按八组组织：
大模型 / 向量 / 记忆 / 主动开口 / 身份演化 / 通道 / 工具 / 程序本身。
前五组可在面板里改（密钥以掩码呈现，审计表不落明文），后三组由 `.env` 决定、只读展示。
桌面 / Linux / Android 共用同一配置契约。
