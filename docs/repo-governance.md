# 仓库治理：审计、守卫与清理

> 本文记录仓库治理机制的调研结论与落地约定。
> 最后更新：2026-11-25（guard 层从 Codex hooks 迁到 Claude Code hooks）

## 结论

没有找到一个同时覆盖「全盘复盘、目录与命名纪律、归属分析、技术债识别、清理」的官方单一 skill。
社区已有多个可组合方案，但通用 skill 往往不了解本仓库的主线边界、文档布局和真实数据保护要求。
因此本仓库采用定制方案：**低频全盘审计 + 每次编辑守卫 + skill/AGENTS 语义约束**。

不采用固定日历定时任务。审计由事件或过期状态触发：

- 重大功能或架构调整完成后；
- 依赖或工具链升级后；
- 自上次审计以来改动量较大（建议约 20 个文件或 500 行）；
- 准备发布或做较大清理前；
- 距上次审计超过约 30 天。

## 三层机制

| 层 | 作用 | 运行时机 | 是否改代码 |
|---|---|---|---|
| `audit` | 全盘盘点目录、命名、归属、重复、死代码、文档漂移、依赖和清理候选 | 手动或上述事件触发 | 默认只读 |
| `guard` | 对编辑动作做确定性的路径、命名和格式检查 | `PreToolUse` / `Stop`（Claude Code hooks） | 只阻断或提示，不自动清理 |
| `skill/AGENTS` | 判断文件应归哪里、模块边界、是否拆分、是否值得修 | 任务开始时或审计时 | 由 agent 按批准的计划执行 |

### audit：低频全盘复盘

使用 `$repo-governance` skill，先运行只读扫描脚本。默认审计不碰真实数据库：

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .agents/skills/repo-governance/scripts/repo_scan.ps1
```

`zhiwei-cli db check` 可用于检查向量签名，但 `Database::open` 会执行幂等 schema 迁移，因此它不能算完全只读。只有用户批准或确实需要核对真实库状态时才运行，并明确说明这一点。扫描输出只作为证据，不把「文件大」直接等同于「应该拆」。报告至少应包含：

- `file:line` 级别的问题；
- 影响范围和严重度；
- 是存量问题还是本次新增；
- 修复成本、风险和验证方式；
- 建议的清理阶段，而不是一个巨大的 diff。

审计报告默认写到 `docs/repo-governance-report.md`，不自动修改代码。

### guard：每次编辑的确定性守卫

守卫已从 Codex hooks 迁到 **Claude Code hooks**（2026-11-25）。项目级配置在 `.claude/settings.json`，脚本在 `.claude/hooks/repo_guard.ps1`（PowerShell 5.1 兼容，UTF-8 BOM 必须保留）。

- `PreToolUse`（matcher `Write|Edit|NotebookEdit`）：拦截主线禁改路径（当前为 `backend/src/channels/ilink/**`），检查新增 Rust 文件 snake_case 命名、`docs/archive/` 目录名英文。阻断以退出码 2 + 简短中文原因。
- `Stop`：工作区有后端 Rust 改动（含未跟踪文件）时运行 `cargo fmt --all -- --check` 与 `cargo check --all-targets`（基线零警告）；只在失败时阻断。

守卫的成功路径保持静默。原 Codex 方案的 PostToolUse 行级检查未迁移；不要在 hook 配置外永久绕过守卫。

注意：若用 shell 命令直接写文件，路径保护不会生效（PreToolUse 只覆盖 Write/Edit/NotebookEdit 工具）；这类操作仍需遵守 `AGENTS.md` 和本 skill 的边界。守卫脚本改动后用 Python `subprocess` 模拟 stdin JSON 回归（正例 + 反例 + 空 stdin 三类）。

### skill：语义判断

skill 放在 `.agents/skills/repo-governance/`。它负责 hook 不适合做的判断：

- 文件应该放在哪个模块；
- 一个文件变大是否意味着职责混杂；
- 哪些测试应该跟随源文件；
- 文档是否过期、是否需要归档；
- 清理是否应该先做、后做，还是暂不做。

skill 的默认流程是 `audit -> report -> user approval -> phased fix`。移动、重命名、删除和批量修改必须得到用户明确批准；优先用 `git mv` 保留历史，并单独提交可回滚的阶段。

## Token 成本

- `PreToolUse` / `Stop` 的确定性命令本身不调用模型，通常不消耗模型 token。
- 只有阻断原因、`additionalContext`、错误日志，或导致模型重试时，才会增加 token。
- 全盘审计会读取更多文件、调用更多推理，成本明显高于单次守卫；因此不要挂在每次编辑上。
- 守卫只回传失败摘要，不输出完整扫描报告；全盘扫描的详细结果写入报告文件，模型只读摘要和相关片段。

## 本仓库边界

- 当前主线是 Telegram；不主动改 `backend/src/api/**` 与 `backend/src/channels/ilink/**`。
- 共享代码可以维护，但只保证 Telegram 端正确。
- `docs/archive/` 的目录名使用英文单词；归档文档不随代码更新。
- 新增环境变量同步到 `backend/.env.example`。
- 不污染真实 `smart_assistant.db`；需要落盘验证时使用临时库并清理测试数据。
- 清理动作不自动执行，不自动提交 Git；提交仍由用户明确发起。

## 参考

- [Claude Code hooks 文档](https://code.claude.com/docs/en/hooks)
- [tech-debt-skill](https://github.com/ksimback/tech-debt-skill)
- [Drakaniia/skills](https://github.com/Drakaniia/skills)
- [repo-hygiene](https://github.com/gg-mo/repo-hygiene)
- [git-repo-hygiene](https://github.com/LinnnnYue/git-repo-hygiene)
- [codemap-skill](https://github.com/Asixa/codemap-skill)

## 启用方式

1. 新开一个 Claude Code 会话，让项目级 `.claude/settings.json` 的 hooks 重新加载。
2. 需要全盘复盘时显式调用 repo-governance skill（`.agents/skills/repo-governance/`），先审计、后报告、再按阶段修复。

