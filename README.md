# 知微 / Zhiwei

本地优先的对话式记忆/知识助手。工程名仍为 `smart-assistant`；品牌名是「知微」——取“见微知著”之意，它从细节中理解并记住你。

核心是一个 Rust 内核（`backend/`），桌面与 Android 只是它的两种消费形态：

- **backend/**：Rust 内核（对话工具循环 + 长期记忆 + 语义检索 + 主动开口 + MCP 客户端）
- **desktop/**：Windows 桌面壳，管理 `zhiwei-service`
- **mcp-servers/**：本机 MCP 服务器（目前是 ComfyUI 出图），被后端按需拉起
- **android/**：Android 端（占位；UniFFI 导出暂已移除，待内核与对话稳定后再接）

## 快速启动（根目录）

日常开发是两个独立进程，各开一个终端（开发态读 `.env.dev`，端口与数据互不抢占）：

```powershell
# 终端 1：后端通道守护 —— 3002，库 backend/smart_assistant.db
cd backend
cargo run --bin zhiwei-service

# 终端 2：桌面 UI —— 不自启上面那个，只识别已运行的进程
cd desktop
npm install        # 仅首次
npm run desktop:dev
```

（MCP 服务器不常驻：后端在 `MCP_ENABLED=1` 时按需拉起 `mcp-servers/` 里配置的
服务器；ComfyUI 出图还需要 ComfyUI 本体在跑。）

一键编排（脚本先拉起 service，再开桌面，退出时统一停止）：

```powershell
make dev
pwsh -NoProfile -File scripts/dev.ps1 -Telegram   # 需要 Telegram 时
```

冒烟与校验（改动完成后跑一遍）：

```bash
make test                           # cargo test（全过 + 零警告是基线）
make smoke                          # 跨进程冒烟：临时库，不碰真实数据

cd backend
cargo check --all-targets
cargo fmt --all -- --check
```

后端配置按环境分文件：开发态读 `backend/.env.dev`，生产态读 `.env.prod`；进程变量
`ZHIWEI_ENV=dev|prod` 决定用哪一份（未设置时 debug 构建取 dev、release 构建取 prod）。

`npm run desktop:dev`（`tauri dev`）在开发态只启动界面，不会自启 `zhiwei-service`
——先手动起它（或用 `make dev` 一键编排），桌面壳会把它显示为“外部启动”。
发布版仍由桌面壳托管自启。

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

默认输出到 `D:\Zhiwei`，入口是 `D:\Zhiwei\zhiwei-desktop.exe`，数据库位于
`D:\Zhiwei\data\smart_assistant.db`。默认只构建并打包运行必需的
`zhiwei-service.exe` 与桌面壳；维护工具不参与日常发布构建。首次创建时会从发布
目录迁移 `.env`、数据库和本地状态；日志从新目录重新开始，避免复制历史敏感信息。
之后只读写新目录。脚本会更新桌面的 `Zhiwei.lnk` 指向新入口；不需要时加
`-NoShortcut`。已有数据不会被删除。
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

## 本机 MCP 服务器（出图）

`mcp-servers/comfy/` 是一个零 pip 依赖的 stdio MCP 服务器，把本机 ComfyUI 的出图
能力暴露成 `mcp__comfy__generate_image` 工具：模型调它，PNG 直接作为 MCP 图片
内容块回传，由后端投给用户（不经过任何网关进程，也不需要 ffmpeg）。

在 `backend/mcp.json` 里配置后端（该文件不入库，模板见 `backend/mcp.json.example`）：

```json
{
  "mcpServers": {
    "comfy": {
      "command": "D:\DevTools\Python312\python.exe",
      "args": ["D:\MyPoject\Rust\smart-assistant\mcp-servers\comfy\main.py"],
      "env": {
        "COMFYUI_URL": "http://127.0.0.1:8188",
        "COMFYUI_WORKFLOW_FILE": "D:\MyPoject\Rust\smart-assistant\mcp-servers\comfy\SD模板2.json",
        "PYTHONUTF8": "1"
      },
      "tools": ["generate_image", "server_info"],
      "timeout_secs": 300
    }
  }
}
```

然后 `MCP_ENABLED=1`，并用 `zhiwei-cli mcp list` / `mcp call` 自查连接与工具表。

开发约定见 [`AGENTS.md`](AGENTS.md)。
