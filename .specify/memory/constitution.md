# smart-assistant Constitution

## Core Principles

### I. 单主线开发（Telegram-Only Mainline）

当前唯一开发主线是 Telegram 通道。

- 任何改动 MUST 以「Telegram 端行为正确」为验收对象。
- 共享模块（`core/`、`llm/`、`db/`、`channels/mod.rs`）照常维护，但 MUST NOT 为迁就
  Web / iLink 的既有行为而回退或添加兼容分支。
- MUST NOT 主动修改 `src/api/**`、`src/channels/ilink/**` 与前端代码；仅当需求明确
  点名时才允许。
- 因共享代码改动导致 Web / iLink 行为变化属预期结果，MUST 写入变更说明，MUST NOT
  自行修复。
- 需要真实通道验证时 MUST 走 `sa-link --telegram`；MUST NOT 起 iLink 或 Web 端做端到端
  验证。单元测试与 `cargo test` 照常全跑。

Rationale：多端共用一套内核，若同时背 Web / iLink 的兼容包袱，重构会持续被拖回旧行为；
把主线收敛到 Telegram，共享代码才敢按正确语义改。

### II. 显式提交（Explicit-Commit Only）

- 只有用户明确说出「提交」「提交 git」「commit」时才允许 `git add` + `git commit`。
- MUST NOT 依据惯性连续提交：一次授权只对应一次提交，后续工作不会自动提交。
- 提交前 MUST 先看 `git status` / `git diff` / `git log --oneline -10`，只暂存本次相关的
  文件，MUST NOT 夹带无关改动或密钥。
- MUST NOT force push、amend，或修改 git 全局 / 局部配置。
- 提交信息 MUST 用中文，按仓库既有风格，一句话概括本次改动。

Rationale：提交是不可逆的对外动作，授权边界必须在指令层面显式，而不是靠 agent 的"惯性"推断。

### III. 绿色基线不可降级（Green Baseline）

任何改动完成后 MUST 同时满足：

- `cargo test` 全过（数据库 + 向量检索冒烟测试）；
- `cargo check --all-targets` 零警告；
- `cargo fmt --all -- --check` 无 diff。

涉及 schema 改动时 MUST 追加 `sa-mem check`（对真实库执行迁移），确认最新版库可正常打开。

排版 MUST 交给 rustfmt 决定，MUST NOT 手写排版；改动写完直接跑 `cargo fmt --all`，否则会在
下次 fmt 时被重排成另一份 diff。

Rationale：零警告与 fmt 归一是一道可机械判定、不可协商的门；一旦允许"下次再修"，它就再也不会被修。

### IV. 真实库不可污染（Never Pollute the Real Database）

`smart_assistant.db` 就是用户的真实数据，`sa-cli` / `sa-web` / `sa-link` 默认都连它。

- 需要真实调用 LLM 或通道来验证功能时，MUST 优先用 `Database::in_memory()`。
- 确需落盘验证时，MUST 改 `SMART_ASSISTANT_DB` 指向临时库，MUST NOT 用默认路径。
- 验证结束 MUST 清理自己造的 `messages` / `memories` / `sessions` / 向量行，并向用户说明
  删了什么；清理 `memories` 时 MUST 同步删 `memory_vectors`（两者按 `memory_id` 关联）。

Rationale：测试对话会经 `settle_memory` 沉淀成记忆并进入向量表，比多几条消息更麻烦——它会
污染后续召回，且污染是静默的。

### V. 最小改动（Minimal Change）

- MUST NOT 主动重构，也 MUST NOT "顺手修好"未被要求触碰的代码。
- 单元测试跟随 Rust 惯例，写在**同文件末尾**的 `#[cfg(test)] mod tests { … }`，不在同目录另开
  测试文件。历史上曾用「独立 `_tests.rs` + `#[path]` 挂载」的写法，这些残留**不强制迁移**，
  在后续迭代里顺手收敛回文件末尾即可。
- 顶层 `tests/` 只放集成测试（走 crate 公开 API）。

Rationale：改动范围可预期，审查聚焦在"改了什么"，而不是被顺手牵连的旁枝。

## 补充约束（文档与环境变量）

- `README.md` 只保留项目介绍（简介 / 组成 / 快速启动）。
- `docs/api-doc.md`（后端接口、数据模型、环境变量配置）与 `docs/architecture.md` MUST 只在
  用户明确要求同步时才更新，不随代码实现变动自动跟进。
- `docs/archive/` 存放已定稿但不再随代码跟进的文档，目录名一律用英文单词，归档文档不再更新。
- `AGENTS.md` 只写长期稳定的硬约束，MUST NOT 在其中冻结随代码演进的架构规则。
- 新增任何环境变量 MUST 同步到 `backend/.env.example`。

## 开发流程与质量门（Development Workflow）

- 常用命令在 `backend/` 下执行：`cargo build --bins`、`cargo test`、
  `cargo check --all-targets`、`cargo clippy --all-targets`、`cargo fmt --all`。
- 进程模型：`sa-web` 与 `sa-link` 是两个独立进程，共享同一份 `.env` 与
  `smart_assistant.db`；主线只需 `sa-link`。
- 非主线入口（`cargo run --bin sa-web`、`make dev`、`make stop`、`scripts/dev.ps1`）
  MUST NOT 用作主线验证手段。
- 规格驱动流程（Spec Kit）中，本宪法是 `plan` / `tasks` / `implement` 的强制约束；与之
  冲突的任务 MUST 先修正规格或任务，MUST NOT 通过稀释原则来放行。

## Governance

- **效力**：本宪法优先于其他开发习惯；出现冲突时以本宪法为准。运行期约定（命令、
  流程细节）以 `AGENTS.md` 为准。
- **修订程序**：任何原则的增删改 MUST 通过一次显式的宪法修订完成，MUST 记录版本变更与
  理由；MUST NOT 在实现类任务中顺手改写原则。
- **版本策略**：语义化版本。MAJOR 为不兼容地删除或重定义原则；MINOR 为新增原则或实质性
  扩展；PATCH 为措辞澄清与非语义修订。
- **合规审查**：每个功能的 `plan` / `tasks` 阶段 MUST 校验是否触及红线（尤其原则
  I、II、IV）；发现偏离 MUST 先修正再进入实现。
- **指导文件**：`AGENTS.md` 是本宪法的运行期指导文件（GUIDANCE_FILE）。

## 修订记录

- **2.0.0（2026-10-19）**：删除原则 V 中「新增单测 MUST 放同目录独立文件」与「MUST NOT 写进逻辑
  代码文件末尾」两条约束，改为跟随 Rust 惯例写在同文件末尾；原则 V 相应更名「最小改动」。
  理由：独立测试文件不符合 Rust 生态惯例，`AGENTS.md` 已同步移除该约定，二者不可长期并存。
  历史遗留的独立 `_tests.rs` 不强制迁移。
- **1.0.0（2026-10-19）**：首次批准。

**Version**: 2.0.0 | **Ratified**: 2026-10-19 | **Last Amended**: 2026-10-19
