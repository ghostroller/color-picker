# color-picker

Windows 原生桌面取色工具，按 [实现计划](docs/implementation-plan.md) 分阶段开发。
技术栈固定为 Rust、`windows` crate、Win32 原生控件和 GDI。

当前进展与验收证据见 [开发记录](docs/progress.md) 和 [验证记录](docs/validation.md)。
当前是开发版本，尚未完成 v0.1 发布验收。

## 构建

需要 Windows x64、Visual Studio C++ Build Tools 和 Windows SDK。
`rust-toolchain.toml` 固定实际使用的 Rust 版本，`Cargo.lock` 固定依赖。

```powershell
cargo run --locked
.\scripts\verify-windows.ps1
```

发布构建位于 `target/x86_64-pc-windows-msvc/release/color-picker.exe`。
非 Windows 平台仅支持 `cargo test --lib --tests --locked` 等纯逻辑检查。

## 范围

目标为 Windows 11 x64，Windows 10 22H2 x64 待兼容性验证。
仅保证普通 SDR 桌面的 8 位 RGB 采样设计，不承诺 HDR、原始 alpha 或受保护内容的颜色。
不联网、不遥测；屏幕图像只保留在内存中。

