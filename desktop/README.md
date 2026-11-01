# Zhiwei Desktop

`zhiwei-desktop` 是知微的 Windows 托盘与本地服务托管壳。它只管理独立的
`zhiwei-service.exe`，不把 Telegram、Agent 或数据库逻辑搬进桌面进程。运行时配置
通过 `zhiwei-service` 的 loopback 控制 API 读写，桌面进程不直连 SQLite。

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
npm run tauri dev
```

不设置时，桌面壳优先寻找可执行文件同目录的 `zhiwei-service.exe`；开发构建会向上查找
仓库里的 `backend/target/release/zhiwei-service.exe` 或 `backend/target/debug/zhiwei-service.exe`。
排查设置窗口时可加 `$env:ZHIWEI_SHOW_ON_START = "1"`，让窗口在启动时显示。

## 当前能力

- 默认只显示托盘，不打开主窗口；
- 左键托盘图标打开状态窗口，右键菜单提供启动、停止、重启、日志、目录和退出；
- 自动启动 `zhiwei-service.exe`，异常退出后按退避间隔重启；
- 单实例运行，重复启动会唤起已有窗口；
- 关闭状态窗口只隐藏，托盘继续运行；
- 构建配置关闭安装器打包，输出仍是可直接运行的便携程序。

状态窗口同时展示运行时配置，保存时使用 revision 乐观锁；启动配置与密钥仍在
`.env`，运行策略已存入数据库，桌面 / Linux / Android 共用同一配置契约。

控制接口启动后写入 `logs/control.json` 与 `logs/control.token`。该接口只绑定
loopback，token 仅由本机进程读取；不要把这两个文件同步到其他机器。

构建不包含安装器的 release 可执行文件：

```powershell
npm run desktop:build
```

或在仓库根目录运行 `pwsh -File scripts/build-portable.ps1` 生成完整的便携目录。

若正式版在窗口出现前退出，初始化错误会写入
`%TEMP%\zhiwei-desktop-panic.log`；服务自身日志仍在程序目录的 `logs/zhiwei-service.log`。
