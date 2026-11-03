# 知微命名与桌面程序规划

> 状态：命名决策已登记；P1 Tauri 桌面壳已实现，等待放行 `zhiwei-service.exe` 后做真实服务验收。
> 日期：2026-11-01。
> 适用范围：Windows 发行体验与用户可见品牌；不改 Telegram 主线内核。
> 关联文档：`README.md`、`scripts/build-portable.ps1`、`scripts/install-release.ps1`、`scripts/release/`。

---

## 1. 命名决策

### 1.1 品牌名

正式品牌名定为 **知微 / Zhiwei**。

命名来源与含义：

- “见微知著”；
- 它从细小对话和长期记忆中理解用户；
- “知”对应记忆与理解，“微”对应细节与个体；
- “Zhiwei”作为国际化拼写，发音简洁，适合作为桌面程序名和发布目录名。

### 1.2 命名层级

本次明确拆分三个层级：

| 层级 | 名称 | 状态 |
|---|---|---|
| 品牌 | 知微 / Zhiwei | 已确定 |
| 工程名 | smart-assistant | 暂不修改 |
| 人格名 | 由身份系统维护 | 不与品牌绑定 |

工程名暂不修改的原因：

1. 当前工作树尚未提交，先全局改名会扩大变更面；
2. `smart_assistant.db`、`zhiwei-service.exe`、crate、文档和环境变量已经形成稳定边界；
3. 本阶段重点是品牌可见性，不是工程资产迁移；
4. 桌面方案确定后再评估是否做一次性工程改名。

### 1.3 本次已改变的可见面

- `README.md` 顶部改为「知微 / Zhiwei」；
- Windows 桌面快捷方式改为 `Zhiwei.lnk`；
- 托盘提示改为「知微 Zhiwei」；
- 设置窗口标题改为「知微 Zhiwei」；
- 正式目录迁移到 `D:\Zhiwei`；`D:\SmartAssistant` 仅保留为旧数据迁移源；
- 数据库和二进制名暂不修改。

### 1.4 后续命名迁移边界

不建议现在直接做以下改动：

- crate / package 全局改名；
- `zhiwei-service` / `zhiwei-cli` 二进制改名；
- `smart_assistant.db` 文件名迁移；
- 旧目录 `D:\SmartAssistant` 的删除；
- 环境变量前缀 `SMART_ASSISTANT_` 改名。

这些改动应等桌面程序架构确定后合并处理，避免为了改名而改名。

---

## 2. 桌面程序问题

### 2.1 过渡形态（P1 前）

当前 Windows 发行体验由 PowerShell 7 和 WinForms 组成：

```text
Zhiwei.lnk
  └─ pwsh.exe -File tray.ps1
      ├─ WinForms NotifyIcon
      ├─ WinForms 右键菜单
      ├─ WinForms 设置窗口
      └─ zhiwei-service.exe 子进程
```

优点：

- 实现快，不需要引入新 UI 技术栈；
- 依赖 Windows 自带 WinForms 能力；
- 已能提供托盘、右键菜单、日志、设置窗口和基础服务管理。

局限：

1. 运行形态是“脚本宿主 + 后台服务”，不如标准桌面程序稳定；
2. UI 与服务管理逻辑写在 PowerShell 中，后续复杂度容易失控；
3. `.env` 明文存储 API key 和 bot token；
4. 安装脚本手工拼接目录、快捷方式和运行时；
5. 对应用控制策略、签名、固定路径等桌面级安全问题没有系统方案。

### 2.2 目标形态

目标是从 PowerShell/WinForms 过渡到 **Tauri 2 桌面壳**：

```text
zhiwei-desktop.exe
  ├─ 托盘图标 / 原生右键菜单
  ├─ 设置窗口（Vite + TypeScript + WebView2）
  ├─ Rust command：配置、日志、服务控制
  └─ zhiwei-service.exe 子进程管理
```

`zhiwei-service.exe` 仍保持独立后台服务，不把 Telegram 轮询、数据库、Agent 主线塞进桌面壳。这样可以把 UI 变更与核心服务解耦，避免桌面程序崩溃影响通道稳定性。

### 2.3 目标能力

- 默认启动到托盘，不弹主窗口；
- 托盘左键或双击打开设置窗口；
- 关闭设置窗口时只隐藏窗口，不退出托盘；
- 右键菜单提供：设置、日志、重启服务、打开数据目录、退出；
- 监控 `zhiwei-service.exe`，异常退出时提示并可自动拉起；
- 单实例运行，重复点击快捷方式时唤起已有窗口；
- 支持开机自启，默认不强制；
- 生成绿色便携目录，双击可执行程序即可运行。

### 2.4 Tauri 技术选择

推荐技术栈：

| 需求 | 方案 |
|---|---|
| 桌面壳 | Tauri 2 |
| 托盘与菜单 | Tauri tray / menu 能力 |
| 设置界面 | Vite + TypeScript + WebView2 |
| 服务管理 | Rust 子进程管理 |
| 单实例 | Windows 命名互斥体 + 本机回环转发 |
| 自启动 | Tauri autostart |
| 发布形态 | 绿色便携目录（`zhiwei-desktop.exe` + `zhiwei-service.exe`） |

不建议使用 Electron：体积和内存开销不适合当前单机长期驻留场景。

### 2.5 配置方案

#### 阶段一：兼容 `.env`

第一版桌面程序继续以 `.env` 为配置源：

```text
设置窗口 → Rust command → 原位修改 .env → 重启 zhiwei-service.exe
```

原因：

- 现有 `zhiwei-service` 已稳定读取 `.env`；
- 可以先完成桌面体验，不改动核心配置加载逻辑；
- 配置迁移可以单独演进，避免第一版桌面壳同时引入配置层改造。

#### 阶段二：结构化配置

后续迁移到：

```text
D:\Zhiwei\config\config.json
Windows Credential Manager / DPAPI
```

原则：

- 非敏感配置放 JSON；
- API key、bot token 等敏感值走系统安全存储；
- `.env` 只作为兼容输入或迁移来源；
- 不把密钥写入日志、设置状态或前端状态。

### 2.6 实施顺序

#### P1：Tauri 桌面壳原型

目标：

- [x] 建立 `desktop/` 目录；
- [x] 实现 Tauri 2 最小可运行窗口；
- [x] 集成托盘图标与右键菜单；
- [x] 启动、停止和监控 `zhiwei-service.exe`；
- [x] 单实例与重复启动唤起窗口；
- [x] 开发版/正式版使用不同的单实例锁和 WebView2 数据目录；
- [ ] 在 Windows Application Control 放行后验证真实 `zhiwei-service.exe`。

验收：

- [x] 双击 `zhiwei-desktop.exe` 进入托盘；
- [ ] 能启动和停止真实服务；
- [x] 重复启动不会产生多个托盘实例。

P1 已完成内容：

- `zhiwei-desktop.exe` 使用 Tauri 2 + WebView2，默认隐藏到托盘；
- 托盘右键菜单包含启动、停止、重启、日志、程序目录和退出；
- `zhiwei-service.exe` 作为独立子进程运行，异常退出后按 5/15/30/60 秒退避恢复；
- 状态窗口展示服务状态、PID、自动恢复次数、程序/配置/日志路径；
- `bundle.active = false`，构建产物是便携目录，不生成安装器；
- `scripts/build-portable.ps1` 可生成直接双击运行的便携目录。

#### P2：设置界面

目标：

- 实现设置窗口；
- 编辑常用配置；
- 保存后原位写回 `.env`；
- 可触发服务重启。

验收：

- 常用配置不再要求手工编辑 `.env`；
- 保存后服务按预期重启；
- 修改失败时不会破坏原 `.env`。

#### P3：日志与诊断

目标：

- 日志窗口或日志文件入口；
- 展示服务 PID、运行状态、最近错误；
- 提供打开数据目录和日志目录的入口。

验收：

- `zhiwei-service.exe` 异常退出时用户能看见原因；
- 日志不泄露 API key 或 bot token。

#### P4：打包与发布

目标：

- 生成绿色便携目录，不做 Windows 安装器；
- 直接双击 `zhiwei-desktop.exe` 启动；
- 保留固定数据目录，程序更新只替换可执行文件；
- 处理 WebView2 缺失时的明确错误提示。

验收：

- 不要求用户执行 PowerShell 安装脚本；
- 便携目录结构简单，可直接复制或压缩备份；
- 升级不覆盖 `.env`、数据库和日志；
- 用户不需要理解安装目录、数据目录和运行时目录的区别。

#### P5：工程改名与最终收敛

目标：

在桌面方案稳定后，统一评估：

- crate 名；
- 二进制名；
- 数据目录；
- 环境变量前缀；
- 发布包名；
- 品牌名与工程名是否收敛。

验收：

- 迁移路径清晰，不破坏旧数据；
- 文档、脚本、发布目录和用户可见名一致。

---

## 3. 当前风险

### 3.1 应用控制策略

当前 Windows 应用控制策略曾阻止执行旧目录中的 `zhiwei-service.exe`。

影响：

- 正式版无法稳定启动；
- 该问题属于系统安全策略，需要用户在 Windows 安全中心处理；
- 后续发布方案必须考虑签名和固定路径受信。

桌面方案要求：

1. `zhiwei-desktop.exe` 和 `zhiwei-service.exe` 使用固定运行路径 `D:\Zhiwei`；
2. 发布版本必须使用稳定签名策略；
3. 不依赖用户手工执行安装目录里的脚本；
4. 快捷方式只指向便携目录内的 `zhiwei-desktop.exe`；
5. 迁移 `.env` 时避免让用户自行下载或复制二进制。

### 3.2 PowerShell 方案的生命周期

PowerShell + WinForms 只是过渡方案，不应继续扩展复杂功能。

后续新增桌面能力应进入 Tauri 方案，不再向 `tray.ps1` / `settings.ps1` 增加复杂逻辑。

---

## 4. 当前结论

1. 品牌名确定为 **知微 / Zhiwei**。
2. 工程名和数据库路径暂不修改，等桌面方案稳定后再合并处理。
3. 当前 PowerShell/WinForms 方案保留为过渡方案，不再继续扩大。
4. 桌面程序长期方案采用 **Tauri 2**。
5. `zhiwei-service.exe` 保持独立后台服务，由 `zhiwei-desktop.exe` 托管。
6. 第一阶段目标是 Tauri 托盘壳与服务管理，不改变 Telegram 主线。
