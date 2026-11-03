# Zhiwei Desktop

`zhiwei-desktop` 是知微的 Windows 托盘与本地服务托管壳。它管理独立的
`zhiwei-service.exe`（通道守护）与可选的 `media-gateway.exe`（本机媒体网关），
不把 Telegram、Agent 或数据库逻辑搬进桌面进程。通道的运行时配置
通过 `zhiwei-service` 的 loopback 控制 API 读写，桌面进程不直连 SQLite。
界面使用 React + Ant Design 5（暗色主题），代码按页面拆分在 `src/pages/`。

## 开发

在同时存在 `backend/.env` 和已编译的 `backend/target/release/zhiwei-service.exe` 时：

```powershell
cd desktop
npm install
npm run desktop:dev
```

开发时可用环境变量指定服务程序和工作目录：

```powershell
$env:ZHIWEI_SERVICE = "D:\MyPoject\Rust\smart-assistant\backend\target\release\zhiwei-service.exe"
$env:ZHIWEI_HOME = "D:\MyPoject\Rust\smart-assistant\backend"
$env:ZHIWEI_MEDIA_GATEWAY = "D:\MyPoject\Rust\smart-assistant\media-gateway\dist\media-gateway.exe"
npm run tauri dev
```

不设置时，桌面壳优先寻找可执行文件同目录的 `zhiwei-service.exe`；开发构建会向上查找
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
- `ZHIWEI_MEDIA_GATEWAY_HOME`：网关工作目录（默认取可执行文件所在目录）；
- `ZHIWEI_MEDIA_GATEWAY_ENV`：网关配置文件路径。

网关配置文件按以下顺序解析：`media-gateway.env`（发布目录推荐）→ 同目录
`.env` → 桌面目录 `.env`（开发模式回退）。状态窗口里的"媒体网关配置"直接读写
这个文件，保存时校验取值并自动重启网关使其生效。界面还会对比网关端口与
`backend/.env` 的 `MEDIA_GATEWAY_URL`，不一致时给出提示（便携发布脚本会自动
把两者对齐）。

控制接口启动后写入 `logs/control.json` 与 `logs/control.token`。该接口只绑定
loopback，token 仅由本机进程读取；不要把这两个文件同步到其他机器。

构建不包含安装器的 release 可执行文件：

```powershell
npm run desktop:build
```

或在仓库根目录运行 `pwsh -File scripts/build-portable.ps1` 生成完整的便携目录。

若正式版在窗口出现前退出，初始化错误会写入
`%TEMP%\zhiwei-desktop-panic.log`；服务自身日志仍在程序目录的 `logs/zhiwei-service.log`。
