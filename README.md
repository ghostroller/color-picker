# color-picker

Windows 原生桌面取色工具，按 [实现计划](docs/implementation-plan.md) 分阶段开发。
技术栈固定为 Rust、`windows` crate、Win32 原生控件和 GDI。

当前进展与验收证据见 [开发记录](docs/progress.md) 和 [验证记录](docs/validation.md)。
当前是开发版本，尚未完成 v0.1 发布验收。

当前已实现 M0 核心与 M1 常驻外壳。启动后在托盘显示图标；`Ctrl + Alt + C`、
托盘激活和重复启动都触发阶段提示。右键菜单提供开始取色、设置、退出。
**当前还不能采样屏幕**，设置入口也仅显示阶段说明；后续按 M2–M6 接入。
退出请使用托盘“退出”。系统可能按通知设置抑制阶段提示。

## 构建

需要 Windows x64、Visual Studio C++ Build Tools 和 Windows SDK。
`rust-toolchain.toml` 固定实际使用的 Rust 版本，`Cargo.lock` 固定依赖。

```powershell
cargo run --locked
.\scripts\verify-windows.ps1
```

在交互 Windows 桌面单独执行常驻外壳冒烟测试（开始前关闭已有 color-picker）：

```powershell
cargo test --locked --test windows_shell -- --ignored --test-threads=1 --nocapture
```

该测试临时启动并关闭自己的实例、占用默认热键以验证冲突，并模拟托盘恢复通知。
不生成真实键鼠输入，也不重启 Explorer；因此不能替代人工热键、菜单和 Explorer 重启验收。
`--check-environment` 检查实际 DPI 上下文后退出；`--diagnostics` 启用按需的宿主状态查询，
不增加后台采样或日志线程。

发布构建位于 `target/x86_64-pc-windows-msvc/release/color-picker.exe`。
非 Windows 平台仅支持 `cargo test --lib --tests --locked` 等纯逻辑检查。

## 范围

目标为 Windows 11 x64，Windows 10 22H2 x64 待兼容性验证。
仅保证普通 SDR 桌面的 8 位 RGB 采样设计，不承诺 HDR、原始 alpha 或受保护内容的颜色。
不联网、不遥测；屏幕图像只保留在内存中。
