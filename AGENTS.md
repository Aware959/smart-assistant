# AGENTS.md

给 AI 编码助手与本仓库开发者的协作约定。这是最高优先级的协作规则，任何时候都要遵守。

## Git 提交规则（最高优先级）

- **必须由用户主动发起「提交 git」才执行 commit**：只有用户明确说出「提交」「提交 git」「commit」时才允许 `git add` + `git commit`。
- **禁止依据惯性连续提交**：用户提过一次「提交」不代表后续工作都自动提交；没有再次明确指示，就不要再 commit。
- 提交前先看 `git status` / `git diff` / `git log --oneline -10`，只暂存本次相关的文件，不夹带无关改动、不带入密钥。
- 不 force push、不 amend、不改 git 全局/局部配置。
- 提交信息用中文、按仓库已有风格，一句话概括本次改动。

## 文档布局

- `README.md`：只保留项目介绍（简介 / 组成 / 快速启动 / Roadmap）。
- `docs/api-doc.md`：后端 HTTP/WS/SSE 接口、数据模型、环境变量配置。
- `AGENTS.md`（本文件）：给 agent/开发者的仓库约定与工作流。
- `backend/.env.example`：环境变量模板。**新增任何环境变量必须同步到这里**。

## 仓库概览

本地优先的对话式记忆/知识助手。Rust 内核 `backend/` + Web 前端 `frontend/` + Android 占位 `android/`。

- `backend/src/lib.rs`：应用外壳（`Assistant` 薄委托）+ 输入输出类型 + UniFFI 导出
- `backend/src/agent.rs`：编排层（单轮对话 / 记忆提取沉淀的业务流水线，不感知 FFI/HTTP）
- `backend/src/services/`：服务层（会话/消息/记忆管理 + FFI 可序列化视图记录）
- `backend/src/db/`：SQLite 层（rusqlite + sqlite-vec 向量检索）
- `backend/src/llm/` + `embedding/` + `genai_client.rs`：模型调用（genai 阻塞 API）
- `backend/src/memory/`：记忆业务层（提取 / 存储 / 召回）
- `backend/src/proactive/`：主动陪伴（调度 / 开口决策 / 推送 / 每轮延续判断）
- `backend/src/channels/`：Telegram / iLink 接入（`PushChannels` 注册表）
- `frontend/`：Vite + React（SSE 流式对话，构建后由后端同源托管）
- `android/`：Android 端（占位，UniFFI Kotlin 绑定）

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

## 关键约定

- **数据库访问**：经 `std::sync::Mutex<db::Database>` 同步。
- **LLM 调用**：genai 是阻塞 API，在 async 通道里调用必须 `tokio::task::spawn_blocking`。
- **消息 vs 记忆**：`messages` 是记录；`memories` 只沉淀事实，来源用 `message_id` 关联。
- **记忆分层**：`tier` = short（短期易过期）/ intent（意向计划）/ core（长期强事实）。short/intent 有 `expires_at`，过期不参与召回；召回按 `MEMORY_RECALL_THRESHOLD`（欧氏距离 ≤ 阈值）过滤；新事实与库中条目距离 ≤ `MEMORY_DEDUP_THRESHOLD` 时去重，只刷新 updated_at 不新增。
- **schema 版本**：版本号在 `db/mod.rs` 的 `SCHEMA_VERSION`。迁移**只做增量**（只增表/列，绝不 DROP 业务表）。旧库补列用 `add_column_if_missing`（以 PRAGMA table_info 判断，且须容忍表不存在——`init_schema` 里 `migrate()` 在业务表 CREATE 之前执行）。
- **sqlite-vec 坑**：vec0 的 kNN 查询必须写 `k = ?` 约束，把 LIMIT 写在外层查询会报 "A LIMIT or 'k = ?' constraint is required"。
- **主动陪伴**：延迟引擎发出的主动消息计入每日配额（sender 的 `count_as_proactive=true`）；延续追加句不算配额。iLink 是窗口式推送：token 新鲜窗口内最多 10 次外发/每次刷新。
- **配置**：全部走环境变量（`config.rs` 读 `backend/.env`，从 `.env.example` 拷贝），不引入配置文件。

## 验证基线

改动完成后：`cargo test` 全过 + `cargo check --all-targets` 零警告。涉及 schema 改动时，还要 `smart-assistant-memory check`（会对真实库执行增量迁移）确认旧库可正常迁移。