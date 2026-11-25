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

## 开发主线（最高优先级）

**当前唯一主线是 Telegram 通道。**

- **Web 端与 iLink 端不要动**：涉及 `src/api/**`（`api/chat.rs`、`api/dto.rs` 等 HTTP/SSE/WS 接口）、
  `src/channels/ilink/**`、前端代码，一律**不主动改**。除非用户明确点名要求开发这两端，
  才允许动。
- **共享代码改动仍要做**：`core/`（agent、prompt、schema、memory）、`llm/`、`db/`、`channels/mod.rs`
  这类被多端共用的模块照常维护，但**只保证 Telegram 端正确**——不为迁就 Web/iLink 的现有行为
  回退或加兼容分支；它们真被要求改时再补。
- **不主动重构、不主动"顺手修好"未被要求触碰的端**。发现 Web/iLink 端因为共享代码改动而
  行为变化，属预期结果，写进说明即可，不要自己去修。
- **验证只跑 Telegram 主线**：需要真实通道验证时用 `zhiwei-service --telegram` 路径，别去起 iLink
  或 Web 端做端到端；单测与 `cargo test` 照常全跑。

## 轻量级后台任务

**轻量级后台任务直接调用本机 `opencode` 执行，不要为此另起 Claude 会话 / 子代理。**

- **适用**：只读、可并行、结论回来后由主线程继续推进的旁路活——批量读文件比对、小范围代码扫描与
  摘要、资料调研、给主线提供候选方案。判据是「改不动主线决策、错了也不致命」。
- **怎么调**：`opencode run --format json --archived "<任务描述>"`（本机已装 opencode v2.4.x，
  在 PATH 上）。`--format json` 出结构化事件流，便于提取最终文本；`--archived` 让脚本化运行
  不污染会话列表；指定模型加 `--model provider/model`。**起在后台跑**，主线程同时推进别的事，
  不要阻塞等它。
- **不适用**：要改本仓代码、要跑 `cargo test` 或影响构建产物、需要多步决策的主线工作——
  这些留在本会话里做，不外包。
- **硬约束**：
  - 外包出去的活**只读**：不得让 opencode 改动仓库文件；Git 提交规则同本文件「Git 提交规则」
    ——只有用户明确说了「提交」才允许 commit。
  - opencode 的输出只是**输入**，不是结论；取舍与最终判断由主线程做。
  - 不要为可选项反复起后台查资料的进程：web 搜索有成本，查十来次仍无果就停下，并在说明里
    写明「未查到」。

## UI 预览验证

- **不要每次都截图**：验证 UI 优先用文本类工具（`preview_snapshot` 看结构、`preview_inspect` 看样式、
  `preview_console_logs` 看报错），截图只在用户要求看效果或纯视觉改动需要展示时才用。
- 浏览器预览入口是 `app/preview.html`（mock 桥，不需要 Tauri 壳）；真机验证才走 `npm run app:dev`。

## 文档布局

- `README.md`：只保留项目介绍（简介 / 组成 / 快速启动）。
- `docs/architecture.md`：架构设计文档。**只有用户明确要求"同步架构文档"时才更新**，不随代码实现变动自动跟进。
- `docs/archive/`：**归档目录，目录名一律用英文单词**。放已定稿但不再随代码跟进的文档
  （架构评审、方案对比、调研结论等）。归档文档**不再更新**——它记录的是某个时间点的
  结论，与当前实现不一致是正常的；要了解现状看 `docs/architecture.md` 和代码。
- `AGENTS.md`（本文件）：给 agent/开发者的仓库约定与工作流。只写稳定硬约束。
- `backend/.env.example`：环境变量模板。**新增任何环境变量必须同步到这里**。

## 常用命令

```bash
cd backend
cargo build --bin zhiwei-service  # 运行主程序；工具按需构建，不随每次发布重编
cargo build --no-default-features --features telegram --bin zhiwei-cli
cargo test                      # 数据库 + 向量检索冒烟测试；全过 + 零警告是基线
cargo check --all-targets       # 确认零警告
cargo clippy --all-targets      # lint
cargo fmt --all                 # 排版（rustfmt，配置见根目录 rust-toolchain.toml）
cargo run --bin zhiwei-service         # 通道守护（Telegram 长轮询 + 主动开口）；主线验证走这条
cargo run --bin zhiwei-cli -- db check          # 巡检向量签名（只读）
cargo run --bin zhiwei-cli -- db rebuild [--force]  # 重建向量表
cargo run --bin zhiwei-cli -- sticker fetch <包名>  # 拉取 Telegram 贴纸包生成表情包目录
cargo run --bin zhiwei-cli -- config get            # 读取数据库运行时配置
cargo run --bin zhiwei-cli -- mcp list              # 查看 MCP 服务器连接状态与工具表
cargo run --bin zhiwei-cli -- mcp call <服> <工具>   # 试调一次 MCP 工具
```

以下命令属于非主线的 Web / iLink 端，主线开发期间不要用来做验证：
`cargo run --bin zhiwei-web`。

进程模型：`zhiwei-web` 与 `zhiwei-service` 是两个独立进程，共享同一份 `.env` / `smart_assistant.db`；
主线只需 `zhiwei-service`。开发 Telegram 时不要用 Web 端做启动方式。

根目录 `make test / tools` 仅保留后端测试与运维 CLI 构建；应用壳发布用 `scripts/build-portable.ps1`。

## 代码约定

- **单元测试跟随 Rust 惯例写在同文件末尾**的 `#[cfg(test)] mod tests { … }`，不在同目录另开测试文件。
  历史上曾用「独立 `_tests.rs` + `#[path]` 挂载」的写法（如 `channels/telegram_tests.rs`），
  这些残留**不强制迁移**，在后续迭代里顺手收敛回文件末尾即可。
- 顶层 `tests/` 只放集成测试（走 crate 公开 API）。

## 验证基线

改动完成后：`cargo test` 全过 + `cargo check --all-targets` 零警告 + `cargo fmt --all -- --check` 无 diff。涉及 schema 改动时，还要 `zhiwei-cli db check`（对真实库执行迁移）确认最新版库可正常打开。

> 注：历史迁移已压平到 v17 基线（见 `docs/architecture.md` §7）。`zhiwei-cli db check` 只对**新库 / 已升到 v17 的库**干净升级；版本号低于 17 的旧库不再支持平滑升级，需 `rm` 后重建。个人单库场景无影响。

**排版由 rustfmt 决定，不要手改**：仓库已全量 `cargo fmt` 归一（`rust-toolchain.toml` 固定 stable + rustfmt/clippy），改动写完直接跑 `cargo fmt --all`。手写排版会在下次 fmt 时被重排成另一种 diff，白白制造冲突。

**验证不许污染真实库**：`smart_assistant.db` 就是用户的真实数据，`zhiwei-cli` / `zhiwei-web` / `zhiwei-service` 也都默认连它。需要真实调用 LLM 或通道来验证功能时：

- 优先用 `Database::in_memory()`（`cargo test` 内部已如此，不碰磁盘库）；
- 确需落盘验证时，改 `SMART_ASSISTANT_DB` 指向临时库跑，别用默认路径；
- 验证结束后清掉自己造的 `messages` / `memories` / `sessions` / 向量行，并告知用户删了什么。

测试对话会经 `settle_memory` 沉淀成 `memories`，还会进向量表——这些会污染后续召回，**比多几条消息更麻烦**。清理时注意 `memories` 与 `memory_vectors` 要同步删，两者按 `memory_id` 关联。
