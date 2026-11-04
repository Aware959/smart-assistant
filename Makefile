# Smart Assistant 根目录便捷命令（Windows，基于 pwsh 与 cargo）
# 需要 GNU make；桌面发布可直接调用 scripts/build-portable.ps1

.PHONY: cert service tools test dev smoke clean help

help:
	@echo "用法: make <target>"
	@echo "  cert     生成（或静默复用）自签名 TLS 证书"
	@echo "  service  前台运行 zhiwei-service（telegram/iLink 通道守护）"
	@echo "  tools    按需构建运维 CLI zhiwei-cli"
	@echo "  test     运行后端单元/集成测试"
	@echo "  dev      开发态一键编排（后端调试构建 + 桌面壳拉起全套）"
	@echo "  smoke    跨进程冒烟（源码态网关 + 临时库 service）"

cert:
	pwsh -NoProfile -File backend/scripts/gen-cert.ps1

service:
	cd backend && cargo run --bin zhiwei-service

tools:
	pwsh -NoProfile -File scripts/build-tools.ps1

test:
	cd backend && cargo test

dev:
	pwsh -NoProfile -File scripts/dev.ps1

smoke:
	pwsh -NoProfile -File scripts/integration-smoke.ps1

clean:
	cd backend && cargo clean
