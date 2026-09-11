# smart-assistant

本地优先的对话式记忆/知识助手。核心是一个 Rust 内核（`backend/`），web 与 Android 只是它的两种消费形态：

- **backend/**：Rust 内核（对话 + 记忆分层 + 语义检索 + 主动陪伴），可选的桌面调试 API（Axum），UniFFI 导出供移动端内嵌
- **frontend/**：Web 端（Vite + React），直接消费 HTTP / SSE 接口
- **android/**：Android 端（占位，经 UniFFI 绑定 + 内嵌 `.so` 调用同一套内核；iOS 不考虑）

## 快速启动（根目录）

```bash
make dev      # 一键启动：自动生成证书（缺失时）+ 后台后端 + 前台前端
make stop     # 停止占用 3000/5173 端口的进程
make build    # 编译后端 + 构建前端
make test     # 后端测试
```

`scripts/dev.ps1` 与 `Makefile` 等价（无 make 时直接 `powershell -File scripts/dev.ps1`）。后端日志写入 `.dev-server.log`；退出 `npm run dev` 时自动停止本次启动的后端。

## 架构（概览）

```
backend/
├── src/
│   ├── lib.rs          应用外壳：Assistant（薄委托）+ 输入输出类型 + UniFFI 导出
│   ├── agent.rs        编排层 Agent：单轮对话 / 记忆提取沉淀的业务流水线
│   ├── services/       服务层：会话/消息/记忆管理 + FFI 视图记录
│   ├── db/             SQLite 层（rusqlite + sqlite-vec 向量检索）
│   ├── llm/            远程 LLM 调用（对话 + 一次性提取/记忆判定）
│   ├── embedding/      文本向量化
│   ├── memory/         记忆业务层：提取 / 存储 / 召回
│   ├── proactive/      主动陪伴：调度 / 开口决策 / 推送 / 延续判断
│   ├── channels/       Telegram / WeChat iLink 接入
│   ├── api/            [desktop] Axum HTTP/WS/SSE 调试接口
│   ├── config.rs       环境变量配置
│   └── error.rs        统一错误 + FFI 错误映射
│
├── tests/              集成测试（冒烟）
└── Cargo.toml          crate 定义（独立 crate）
```

## Roadmap

- [x] Rust 内核：对话 / 记忆分层 / 向量检索
- [x] 桌面调试 API（Axum）
- [x] Web 端（Vite + React，SSE 流式对话）
- [x] Telegram / WeChat 主动陪伴（不规律主动 + 每轮延续判断）
- [ ] Android Gradle 工程 + UniFFI Kotlin 绑定 + `.so` 接入
- [ ] 对话流式输出与后台 LLM 任务队列（桌面端已就绪）

接口与配置详见 [`docs/api-doc.md`](docs/api-doc.md)；开发约定见 [`AGENTS.md`](AGENTS.md)。