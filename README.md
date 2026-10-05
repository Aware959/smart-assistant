# smart-assistant

本地优先的对话式记忆/知识助手。核心是一个 Rust 内核（`backend/`），web 与 Android 只是它的两种消费形态：

- **backend/**：Rust 内核（对话工具循环 + 长期记忆 + 语义检索 + 主动开口），可选的桌面调试 API（Axum）
- **frontend/**：Web 端（Vite + React），直接消费 HTTP / SSE 接口
- **android/**：Android 端（占位；UniFFI 导出暂已移除，待内核与对话稳定后再接）

## 快速启动（根目录）

```bash
make dev      # 一键启动：自动生成证书（缺失时）+ 后台后端 + 前台前端
make stop     # 停止占用 3000/5173 端口的进程
make build    # 编译后端 + 构建前端
make test     # 后端测试
```

`scripts/dev.ps1` 与 `Makefile` 等价（无 make 时直接 `powershell -File scripts/dev.ps1`）。后端日志写入 `.dev-server.log`；退出 `npm run dev` 时自动停止本次启动的后端。

接口与配置详见 [`docs/api-doc.md`](docs/api-doc.md)；开发约定见 [`AGENTS.md`](AGENTS.md)。