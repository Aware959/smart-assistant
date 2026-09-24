# Smart Assistant 根目录便捷命令（Windows，基于 pwsh 与 npm/cargo）
# 需要 GNU make；也可直接调用 scripts/dev.ps1 / stop.ps1

.PHONY: cert build server web link dev stop test clean help

help:
	@echo "用法: make <target>"
	@echo "  cert     生成（或静默复用）自签名 TLS 证书"
	@echo "  build    编译后端全部 bin + 构建前端"
	@echo "  server   前台运行 sa-web（HTTP 会话主进程，https://127.0.0.1:3000）"
	@echo "  link     前台运行 sa-link（telegram/iLink 通道守护）"
	@echo "  web      前台运行前端 vite dev (http://localhost:5173)"
	@echo "  dev      一键启动 sa-web + 前端（后端后台、前端前台，退出时自动清理）"
	@echo "  stop     停止占用 3000/5173 端口的 sa-web 与 vite 进程，并清理 sa-link"
	@echo "  test     运行后端单元/集成测试"

cert:
	pwsh -NoProfile -File backend/scripts/gen-cert.ps1

build:
	cd backend && cargo build --bins
	cd frontend && npm run build

server:
	cd backend && cargo run --bin sa-web

link:
	cd backend && cargo run --bin sa-link

web:
	cd frontend && npm run dev

dev:
	pwsh -NoProfile -File scripts/dev.ps1

stop:
	pwsh -NoProfile -File scripts/stop.ps1

test:
	cd backend && cargo test

clean:
	cd backend && cargo clean
	cd frontend && if exist node_modules rmdir /s /q node_modules