# color-picker

Windows 原生桌面取色工具，按 [实现计划](docs/implementation-plan.md) 分阶段开发。
技术栈固定为 Rust、`windows` crate、Win32 原生控件和 GDI。

当前进展与验收证据见 [开发记录](docs/progress.md) 和 [验证记录](docs/validation.md)。
当前是开发版本，尚未完成 v0.1 发布验收。

当前已实现 M0–M5：实时取色、冻结放大、结果展示和复制。启动后在托盘显示图标；
按 `Ctrl + Alt + C`、激活托盘或重复启动，开始显示鼠标所在物理像素的颜色、HEX 和坐标。
取色中重复激活不会叠加会话；右键或 Esc 取消，退出程序请使用托盘菜单。
鼠标静止时仍检查画面变化，停止预览后释放采样资源和定时器。
左键确认颜色；取色期间消费鼠标点击和滚轮，结束后释放输入钩子。
滚轮向上冻结并放大，倍率为 4× / 8× / 16× / 32×；向下滚出 4× 恢复实时取色。
冻结后在像素格内点击确认，边框 / 文字栏 / 留白点击不取色。
确认后打开原生结果窗口，显示 HEX、RGB、CSS RGB、HSL、原始坐标及实时 / 冻结来源。
每行可单独复制，也可复制默认 HEX；支持 Tab、Enter、Esc、文本选择和“重新取色”。
默认不自动复制。自定义快捷键、默认格式、自动复制和配置保存属于后续 M6。

## 构建

需要 Windows x64、Visual Studio C++ Build Tools 和 Windows SDK。
`rust-toolchain.toml` 固定实际使用的 Rust 版本，`Cargo.lock` 固定依赖。

```powershell
cargo run --locked
.\scripts\verify-windows.ps1
```

在交互 Windows 桌面单独执行桌面测试（开始前关闭已有 color-picker）：

```powershell
cargo test --locked --test windows_capture --test windows_preview --test windows_shell --test windows_selection_ui -- --ignored --test-threads=1 --nocapture
```

这些测试显示小型已知像素窗口和预览，检查采样、非激活窗口、资源释放以及停止后不再采样；
临时启动并关闭自己的实例、占用默认热键验证冲突，并模拟托盘恢复及显示变化消息。
宿主测试会短暂安装取色钩子；结果窗口测试核对原生控件和文本，不改剪贴板。
不生成真实键鼠输入，也不重启 Explorer；不能替代人工热键、菜单、多屏 DPI 和 Explorer 重启验收。
运行时请保持测试窗口无遮挡，不切换前台程序或修改显示设置。

人工核对像素可运行 `cargo run --locked --example pixel-fixture`：
窗口客户区为 384×256 像素，局部 `(x, y)` 的 RGB 为 `(x % 256, y % 256, (x ^ y) % 256)`；
底部 16 行改为每列循环红、绿、蓝的单像素条纹。标题显示客户区物理原点，关闭窗口结束。
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
| `activation.handled` | 已处理激活，开始实时预览 |
| `activation.ignored` | 预览已开启，忽略重复激活 |
| `preview.started` | 采样会话及定时器已建立 |
| `preview.stopped` | 会话结束，定时器和预览资源已释放 |
| `session.starting` / `session.finishing` | 等待输入就绪 / 正在释放已消费的手势 |
| `session.frozen` / `session.resumed_live` | 进入冻结放大 / 恢复实时取色 |
| `result.shown` | 输入与采样资源清理完毕，结果窗口已显示 |
| `preview.sample_unavailable` / `preview.failed` | 采样暂不可用或预览因错误停止 |
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
