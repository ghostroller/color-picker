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

## 快捷键无响应时的日志

先从托盘退出已有实例，然后运行：

```powershell
.\scripts\start-with-logs.ps1
```

脚本将构建到独立的 `target/diagnostic` 目录，启动带日志的程序，并输出日志路径及
实时查看命令。每次启动在 `logs/` 中创建独立文件，不会关闭已有实例。
**如果旧实例未退出，新进程只激活旧实例后退出，无法给旧版本补开日志。**

也可以为 EXE 显式指定 `--log-file <路径>`，可与 `--check-environment` 或 `--diagnostics` 组合。
`--diagnostics` 单独使用仍只开启状态查询，不写日志。Release 没有控制台，文件日志可直接读取。

| 日志事件 | 含义 |
|---|---|
| `hotkey.registered` | Ctrl+Alt+C 已成功注册 |
| `hotkey.registration_failed` | 注册失败，后面包含系统错误；可能已被其他程序占用 |
| `hotkey.received` | 宿主已收到该快捷键的 WM_HOTKEY |
| `activation.handled` | 已处理激活；当前 M1 仅发阶段通知 |
| `tray.notification_accepted` | Windows 已接受通知请求，不保证用户看到了通知 |
| `tray.balloon_show` | 收到 Shell 的通知显示回调 |
| `instance.existing` | 发现旧实例，本次日志不会记录旧进程中的快捷键 |

默认不写日志。诊断日志仅记录离散应用事件和错误，不记录一般按键、屏幕像素或剪贴板内容；
没有日志线程或刷新定时器。单个日志上限 1 MiB，达到上限写入 `log.limit_reached` 后停止记录；
此时换一个文件路径或重新运行脚本。

## 范围

目标为 Windows 11 x64，Windows 10 22H2 x64 待兼容性验证。
仅保证普通 SDR 桌面的 8 位 RGB 采样设计，不承诺 HDR、原始 alpha 或受保护内容的颜色。
不联网、不遥测；屏幕图像只保留在内存中。
