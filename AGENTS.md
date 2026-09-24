# AGENTS.md

给 AI 编码助手与本仓库开发者的协作约定。这是最高优先级的协作规则，任何时候都要遵守。

> 本文件只写**长期稳定的硬约束**。架构规则不在这里冻结——它们随代码演进，
> 写进约定只会让文档在每次重构后失真。

## Git 提交规则（最高优先级）

- **必须由用户主动发起「提交 git」才执行 commit**：只有用户明确说出「提交」「提交 git」「commit」时才允许 `git add` + `git commit`。
- **禁止依据惯性连续提交**：用户提过一次「提交」不代表后续工作都自动提交；没有再次明确指示，就不要再 commit。
- 提交前先看 `git status` / `git diff` / `git log --oneline -10`，只暂存本次相关的文件，不夹带无关改动、不带入密钥。
- 不 force push、不 amend、不改 git 全局/局部配置。
- 提交信息用中文、按仓库已有风格，一句话概括本次改动。

## 文档布局

- `README.md`：只保留项目介绍（简介 / 组成 / 快速启动）。
- `docs/api-doc.md`：后端 HTTP/WS/SSE 接口、数据模型、环境变量配置。**只有用户明确要求同步时才更新**，不随代码实现变动自动跟进。
- `docs/architecture.md`：架构设计文档。**只有用户明确要求"同步架构文档"时才更新**，不随代码实现变动自动跟进。
- `AGENTS.md`（本文件）：给 agent/开发者的仓库约定与工作流。只写稳定硬约束。
- `backend/.env.example`：环境变量模板。**新增任何环境变量必须同步到这里**。

## 常用命令

```bash
cd backend
cargo build --bins             # 一次编译 sa-web / sa-link / sa-cli / sa-mem
cargo test                      # 数据库 + 向量检索冒烟测试；全过 + 零警告是基线
cargo check --all-targets       # 确认零警告
cargo clippy --all-targets      # lint
cargo run --bin sa-web          # HTTP 会话主进程（Web API + 前端静态托管）
cargo run --bin sa-link         # 通道守护（Telegram / iLink 长轮询 + 主动开口）
cargo run --bin sa-cli [-- "一句话"]
cargo run --bin sa-mem -- check          # 巡检向量签名（只读）
cargo run --bin sa-mem -- rebuild [--force]  # 重建向量表
```

进程模型：`sa-web` 与 `sa-link` 是两个独立进程，共享同一份 `.env` / `smart_assistant.db`，
可按需只跑其一；`docs/api-doc.md` 里 Smart Assistant → sa-web（HTTP 接口）。
根目录 `make dev` 只启动 `sa-web` 与前端；`sa-link` 在 `backend/.env` 里配置了
Telegram token 或 `ILINK_ENABLED=1` 时由 `make dev` 一并后台拉起。

根目录 `make dev / stop / build / test` 与 `scripts/dev.ps1` 等价（无 make 用 `powershell -File scripts/dev.ps1`）。

## 代码约定

- **单元测试不写进逻辑代码文件末尾**。新单元测试放同目录独立文件（如 `core/memory/current_relation.rs` 的测试 → `core/memory/current_relation_tests.rs`），原文件末行用 `#[cfg(test)] #[path = "<name>_tests.rs"] mod <name>_tests;` 挂载（`#[path]` 指到同级文件，否则子模块会去 `<file>/<name>_tests.rs` 目录里找），测试内用 `super::` 访问原模块（私有成员可及性不变）。
- 顶层 `tests/` 只放集成测试（走 crate 公开 API）。历史遗留的内嵌 `#[cfg(test)] mod tests` 不强制迁移。

## 验证基线

改动完成后：`cargo test` 全过 + `cargo check --all-targets` 零警告。涉及 schema 改动时，还要 `sa-mem check`（会对真实库执行增量迁移）确认旧库可正常迁移。