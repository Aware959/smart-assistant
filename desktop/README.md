# Zhiwei Desktop

`zhiwei-desktop` 是知微的 Windows 托盘与本地服务托管壳。它管理独立的
`zhiwei-service.exe`（通道守护）与可选的 `media-gateway.exe`（本机媒体网关），
不把 Telegram、Agent 或数据库逻辑搬进桌面进程。通道的运行时配置
通过 `zhiwei-service` 的 loopback 控制 API 读写，桌面进程不直连 SQLite。
界面使用 React + Ant Design 5（暗色主题），代码按页面拆分在 `src/pages/`。

## 开发

一键编排（编译后端调试构建，脚本先拉起 service 与源码态媒体网关，再启动
`tauri dev`；桌面壳在开发态只当 UI，不自启这两个进程）：

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

# 终端 2：媒体网关（源码态）
cd media-gateway
python main.py

# 终端 3：桌面 UI
cd desktop
npm install
npm run desktop:dev
```

`tauri dev` **不会**自动启动 service 与媒体网关。桌面壳通过 `logs/control.json`
里的 pid 和网关的 `/health` 识别外部进程，界面显示“已在运行（外部启动）”，
不会重复拉起；这类进程也不归桌面壳停止 / 重启，请在启动它的终端里操作。
发布版（release 构建）仍由桌面壳托管自启。

开发时可用环境变量指定服务程序和工作目录：

```powershell
$env:ZHIWEI_SERVICE = "D:\MyPoject\Rust\smart-assistant\backend\target\release\zhiwei-service.exe"
$env:ZHIWEI_HOME = "D:\MyPoject\Rust\smart-assistant\backend"
$env:ZHIWEI_MEDIA_GATEWAY = "D:\MyPoject\Rust\smart-assistant\media-gateway\dist\media-gateway.exe"
npm run tauri dev
```

不设置时，桌面壳优先寻找可执行文件同目录的 `zhiwei-service.exe`；开发构建会向上查找
仓库里的 `backend/target/release/zhiwei-service.exe` 或 `backend/target/debug/zhiwei-service.exe`。
媒体网关同理：找不到 `media-gateway.exe` 时，会回退到仓库里的 `media-gateway/main.py`
以源码态运行（优先 `media-gateway/.venv` 的解释器，其次 PATH 中的 `python`）。
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
- 同目录存在 `media-gateway.exe` 时自动启动媒体网关，并做同样的退避恢复与
  `/health` 健康检查；不存在时界面显示"未安装"，不影响通道；
- 单实例运行，重复启动会唤起已有窗口；
- 关闭状态窗口只隐藏，托盘继续运行；
- 构建配置关闭安装器打包，输出仍是可直接运行的便携程序。

状态窗口同时展示运行时配置，保存时使用 revision 乐观锁；启动配置与密钥仍在
`.env`，运行策略已存入数据库，桌面 / Linux / Android 共用同一配置契约。

## 媒体网关（media-gateway）

媒体网关是可选组件：把 `media-gateway.exe` 放在 `zhiwei-desktop.exe` 同目录（或
`media-gateway/` 子目录），桌面壳会自动启动并在托盘提供启动 / 重启 / 停止 /
日志 / 配置入口。开发时可分别用环境变量覆盖：

- `ZHIWEI_MEDIA_GATEWAY`：网关可执行文件路径；
- `ZHIWEI_MEDIA_GATEWAY_PYTHON`：源码态运行时使用的 Python 解释器（默认优先
  `media-gateway/.venv`，再退回 PATH 中的 `python`）；
- `ZHIWEI_MEDIA_GATEWAY_HOME`：网关工作目录（默认取可执行文件所在目录）；
- `ZHIWEI_MEDIA_GATEWAY_ENV`：网关配置文件路径。

网关配置文件按以下顺序解析：`ZHIWEI_MEDIA_GATEWAY_ENV` → `.env.<ZHIWEI_ENV>`
（开发 `media-gateway/.env.dev` / 生产 `.env.prod`）→ `media-gateway.env`
（发布目录推荐）→ 同目录 `.env` → 桌面目录 `.env`（开发模式回退）。状态窗口里的
"媒体网关配置"直接读写这个文件，保存时校验取值并自动重启网关使其生效。界面还会
对比网关端口与 `backend/.env.dev` / `.env.prod` 的 `MEDIA_GATEWAY_URL`，不一致时
给出提示（便携发布脚本会自动把两者对齐）。

控制接口启动后写入 `logs/control.json` 与 `logs/control.token`。该接口只绑定
loopback，token 仅由本机进程读取；不要把这两个文件同步到其他机器。

构建不包含安装器的 release 可执行文件：

```powershell
npm run desktop:build
```

或在仓库根目录运行 `pwsh -File scripts/build-portable.ps1` 生成完整的便携目录。

若正式版在窗口出现前退出，初始化错误会写入
`%TEMP%\zhiwei-desktop-panic.log`；服务自身日志仍在程序目录的 `logs/zhiwei-service.log`。
