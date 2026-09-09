# Android 端（占位）

本目录预留 Android 移动端入口。iOS 不考虑。

## 计划结构

```
android/
├── app/                     # Gradle 应用模块
│   ├── src/main/java/…/     # Kotlin 代码（经 UniFFI 绑定调用 Rust 内核）
│   └── src/main/jniLibs/    # 各 ABI 的 smart_assistant.so
├── bindings/kotlin/         # uniffi-bindgen 生成的 Kotlin 绑定
└── build.gradle / settings.gradle / gradle.properties
```

## 接入方式

1. **生成绑定**（宿主即可，绑定与目标平台无关）：

   ```bash
   uniffi-bindgen generate --library ../backend/target/debug/smart_assistant.dll --language kotlin --out-dir ../android/bindings/kotlin
   ```

2. **交叉编译 `.so`**（需 Android NDK + cargo-ndk）：

   ```bash
   cargo install cargo-ndk
   rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
   cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 build --release
   ```

   产物复制到 `app/src/main/jniLibs/{arm64-v8a,armeabi-v7a,x86_64}/libsmart_assistant.so`。

3. Kotlin 侧：`bindings.loadIndirect()` 得到 `Assistant` 对象，调用 `chatStream` 走流式回调。

## 状态

- [ ] 目录骨架与 Gradle 工程
- [ ] 生成 Kotlin 绑定
- [ ] cargo-ndk 交叉编译 `.so`
- [ ] 示例 ChatActivity + 流式输出