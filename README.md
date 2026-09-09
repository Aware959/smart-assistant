# smart-assistant

本地优先的对话式记忆/知识助手。核心是一个 Rust 内核（`backend/`），web 与 Android 只是它 的两种消费形态：

- **backend/**：Rust 内核（对话 + 记忆 + 知识图谱 + 语义检索），可选的桌面调试 API（Axum），UniFFI 导出供移动端内嵌
- **frontend/**：Web 端（Vite + React），直接消费 HTTP / SSE 接口
- **android/**：Android 端（占位，经 UniFFI 绑定 + 内嵌 `.so` 调用同一套内核；iOS 不考虑）

## 架构

```
backend/
├── src/
│   ├── lib.rs          应用外壳：Assistant（薄委托）+ 输入输出类型 + UniFFI 导出
│   ├── agent.rs        编排层 Agent：单轮对话 / 提取落库 的业务流水线（不感知 FFI/HTTP）
│   ├── services/       服务层：会话/消息/记忆/图谱 管理 + FFI 可序列化视图记录类型
│   ├── db/             SQLite 层（rusqlite + sqlite-vec 向量检索）
│   │   ├── session.rs  会话 CRUD
│   │   ├── message.rs  消息（会话内 user/assistant 记录）
│   │   ├── memory.rs   记忆（事实）CRUD + KNN 语义检索
│   │   └── relation.rs 实体 / 关系图
│   ├── llm/            远程 LLM 调用（对话 + 一次性提取/记忆判定）
│   ├── embedding/      文本向量化
│   ├── genai_client.rs genai 适配层：两套 Client（chat/embed）+ 内置 tokio runtime
│   ├── extractor/      LLM 输出 → 结构化 记忆决策 + Entity/Relation 解析
│   ├── memory/         业务层：记忆存储 + 关系图操作
│   ├── api/            [desktop] Axum HTTP/WS 调试接口
│   ├── config.rs       环境变量配置
│   └── error.rs        统一错误 + FFI 错误映射
│
├── tests/              集成测试（冒烟）
└── Cargo.toml          crate 定义（独立 crate）
```

## 功能

- **对话**：消息按会话记录（`messages`），LLM 一次性判定是否值得记住并提取实体/关系
- **记忆**：只沉淀事实（`memories`），与消息记录分离；来源通过 `message_id` 关联
- **会话**：后端自动创建 session，前端保存 `session_id` 后可跨轮/跨端续聊
- **关系图**：实体去重、关系累加权重、查询与删除
- **跨端**：`cdylib` + UniFFI，同一套内核导出 Kotlin 绑定

## 数据模型

| 表 | 内容 | 说明 |
|----|------|------|
| sessions | 会话（id / title / 时间戳） | 标题来自首条消息 |
| messages | 消息记录（role: user/assistant） | 只作记录，不一定产生记忆 |
| memories | 记忆/事实（content / memory_type / message_id） | LLM 判定 `is_memory` 时才沉淀 |
| entities / relations | 知识图谱 | 关联到其来源记忆 |

对话流程：用户消息 → 一次 LLM 调用（判定 `is_memory`、事实化内容、实体/关系）→ 记忆向量检索 + 图谱召回 → 重组提示词 → 回复 → 消息入库；LLM 判定值得记住时才写入 `memories`。

## 运行

### 桌面调试（backend）

```bash
# 必备：LLM 与 embedding 的 API Key
$env:LLM_API_KEY = "sk-..."
$env:EMBEDDING_API_KEY = "sk-..."

cd backend
cargo run --bin smart-assistant-server
# 服务地址: http://127.0.0.1:3000  (可用 SMART_ASSISTANT_ADDR 覆盖)
# 数据库文件: backend/smart_assistant.db (可用 SMART_ASSISTANT_DB 覆盖)

# 命令行交互式聊天（流式输出）
cargo run --bin smart-assistant-cli
# 单发问答
cargo run --bin smart-assistant-cli -- "一句话"
```

### Web 端（frontend）

```bash
cd frontend
npm install
npm run dev        # http://localhost:5173，/chat、/sessions 等代理到 3000
npm run build      # 产物在 frontend/dist（路由级 code-split）
```

技术栈：Vite 8 + React 19 + TypeScript（strict）+ Tailwind v4（shadcn/ui 组件体系）+ assistant-ui（`useLocalRuntime` 流式对话 + react-markdown 渲染）+ TanStack Query + React Router + Zustand + React Hook Form + zod + axios + sonner。

打开页面：新建会话 → 输入 → 助手回复实时流式呈现；左侧可切换会话 / 记忆 / 图谱。生产部署时将 `dist` 放到任意静态服务器，并让 `/chat`、`/sessions`、`/memories`、`/search` 等路径反代到 backend（或给 backend 加 CORS）。

手机访问（同一局域网）：`vite` 已配置 `host: true`（监听所有网卡），手机浏览器打开 `http://<电脑内网IP>:5173` 即可；`/chat`、`/sessions` 等仍由 vite 在本机代理到 backend，无需改动后端。若仍连不上，检查 Windows 防火墙是否放行 5173 端口入站（私有网络），并确认手机与电脑在同一网段。

## API

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | /chat/stream | 对话 `{message, session_id?, history_count?}`；`session_id` 缺省自动新建；上下文自动取该会话最近 `history_count` 条历史消息（缺省 6）；回复以 **SSE** 实时推送，格式与 OpenAI `chat.completion.chunk` 兼容 |
| POST | /sessions | 手动新建会话 |
| GET  | /sessions | 列出会话（按最近更新倒序） |
| GET  | /sessions/{id}/messages | 某会话的消息记录 |
| DELETE | /sessions/{id} | 删除会话及消息（记忆保留） |
| POST | /memory | 手动添加记忆 `{content, memory_type?, message_id?}`（缺省 fact） |
| GET  | /memories | 列出记忆 |
| DELETE | /memory/{id} | 删除记忆 |
| POST | /search | 语义检索 `{query, limit?}` |
| GET  | /entities | 列出实体 |
| GET  | /relations | 列出关系 |
| DELETE | /relations/{id} | 删除关系 |
| DELETE | /entities/{id} | 删除实体 |
| GET  | /ws | WebSocket 对话：回复文本逐段实时下发，结束时报完整 `ChatOutput` JSON |

### 上下文构建

- 每轮对话自动从数据库取出该会话**最近的历史消息**拼入提示词（含历史的 assistant 回复）；
- 数量由请求参数 `history_count` 控制，`0` 表示不带历史，缺省 `6`；
- 请求里也可显式传 `history`（`[{role, content}]`）作为补充上下文，会拼在自动历史之后。

### SSE 实时输出（/chat/stream，OpenAI 兼容）

```bash
curl -N -X POST http://127.0.0.1:3000/chat/stream \
  -H "Content-Type: application/json" \
  -d '{"message":"你好"}'
```

- 常规数据行均为 OpenAI `chat.completion.chunk`：`data: {"choices":[{"delta":{"content":"..."},...}],"object":"chat.completion.chunk",...}`
- 结尾 `data: [DONE]`（OpenAI SDK / EventSource 可直接按标准流解析）
- 附加元数据事件 `event: done`：`data: <ChatOutput JSON>`（含 `session_id` / `memory` / `entities` / `relations`，非标准扩展，标准客户端会忽略）
- 出错时流内返回 `data: {"error":{"message":"...","type":"assistant_stream_error"}}`，随后仍有 `[DONE]` 与 `done` 事件

## 配置（环境变量）

| 变量 | 默认值 |
|------|--------|
| LLM_API_URL | http://127.0.0.1:1234/v1/completions |
| LLM_API_KEY | (空) |
| LLM_MODEL | google/gemma-4-26b-a4b-qat |
| EMBEDDING_API_URL | http://127.0.0.1:1234/v1/embeddings |
| EMBEDDING_API_KEY | (空) |
| EMBEDDING_MODEL | text-embedding-embeddinggemma-300m |
| EMBEDDING_DIM | 768 |
| SMART_ASSISTANT_DB | smart_assistant.db（相对 backend/ 运行目录） |
| SMART_ASSISTANT_ADDR | 127.0.0.1:3000 |

LLM URL 可指向任何 OpenAI 兼容接口（本地 Ollama / 其它厂商网关）。LLM 与 embedding 调用统一走 [genai](https://crates.io/crates/genai) 适配层（多厂商、OpenAI 兼容优先）；API Key 留空时回退读取 `OPENAI_API_KEY`。

## 测试

```bash
cd backend
cargo test          # 数据库 + 向量检索 + 关系图冒烟测试
cargo clippy --all-features --all-targets   # lint（目标零警告）
```

## Android（占位，见 android/README.md）

1. 生成 Kotlin 绑定（宿主即可）：`uniffi-bindgen generate --library target/debug/smart_assistant.dll --language kotlin --out-dir ../android/bindings/kotlin`
2. `cargo install cargo-ndk` + `rustup target add aarch64-linux-android …` 交叉编译 `.so`
3. Android Gradle 引入 `.so` + Kotlin 绑定，直接调用 `Assistant`

## Roadmap

- [x] Rust 内核：对话 / 记忆 / 实体关系 / 向量检索
- [x] 桌面调试 API（Axum）
- [x] Web 端（Vite + React，SSE 流式对话）
- [ ] Android Gradle 工程 + UniFFI Kotlin 绑定 + `.so` 接入
- [ ] 对话流式输出与后台 LLM 任务队列（桌面端已就绪）
- [ ] 迁移策略（embedding 维度变更时重建 vec0 表）