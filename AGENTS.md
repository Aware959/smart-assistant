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
cargo build
cargo test                      # 数据库 + 向量检索冒烟测试；全过 + 零警告是基线
cargo check --all-targets       # 确认零警告
cargo clippy --all-targets      # lint
cargo run --bin smart-assistant-server
cargo run --bin smart-assistant-cli [-- "一句话"]
cargo run --bin smart-assistant-memory -- check          # 巡检向量签名（只读）
cargo run --bin smart-assistant-memory -- rebuild [--force]  # 重建向量表
```

根目录 `make dev / stop / build / test` 与 `scripts/dev.ps1` 等价（无 make 用 `powershell -File scripts/dev.ps1`）。

## 验证基线

改动完成后：`cargo test` 全过 + `cargo check --all-targets` 零警告。涉及 schema 改动时，还要 `smart-assistant-memory check`（会对真实库执行增量迁移）确认旧库可正常迁移。