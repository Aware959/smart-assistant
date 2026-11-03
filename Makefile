# Smart Assistant 根目录便捷命令（Windows，基于 pwsh 与 cargo）
# 需要 GNU make；桌面发布可直接调用 scripts/build-portable.ps1

.PHONY: cert service tools test clean help

help:
	@echo "用法: make <target>"
	@echo "  cert     生成（或静默复用）自签名 TLS 证书"
	@echo "  service  前台运行 zhiwei-service（telegram/iLink 通道守护）"
	@echo "  tools    按需构建运维 CLI zhiwei-cli"
	@echo "  test     运行后端单元/集成测试"

cert:
	pwsh -NoProfile -File backend/scripts/gen-cert.ps1

service:
	cd backend && cargo run --bin zhiwei-service

tools:
	pwsh -NoProfile -File scripts/build-tools.ps1

test:
	cd backend && cargo test

clean:
	cd backend && cargo clean
