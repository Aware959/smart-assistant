# Android 端（占位）

目录预留 Android 移动端位置。iOS 不考虑。

## 状态

研发早期先稳定对话与内核：**UniFFI 导出已移除**（`backend` 的 `crate-type` 改回纯 rlib，
无 cdylib，也无 Kotlin 绑定/`.so`），本目录保持占位，不接移动端。

## 计划结构（届时重建）

```
android/
├── app/                     # Gradle 应用模块
│   ├── src/main/java/…/     # Kotlin 代码（经 UniFFI 绑定调用 Rust 内核）
│   └── src/main/jniLibs/    # 各 ABI 的 smart_assistant.so
├── bindings/kotlin/         # uniffi-bindgen 生成的 Kotlin 绑定
└── build.gradle / settings.gradle / gradle.properties
```

接入路径（内核稳定后）：

1. 恢复 `backend` 的 `uniffi` 依赖与 cdylib 产物；
2. `uniffi-bindgen generate --library ../backend/target/debug/smart_assistant.dll --language kotlin`；
3. Android NDK + cargo-ndk 交叉编译 `.so`；
4. Kotlin 侧加载绑定，调用 `Assistant` 的对话能力。

## 现状 TODO

- [ ] 内核与对话稳定（当前阶段目标）
- [ ] 恢复 UniFFI 导出与 Kotlin 绑定
- [ ] Gradle 工程 + 示例 ChatActivity