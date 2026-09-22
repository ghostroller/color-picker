# color-picker 实现路径

**文档版本：1.0 · 制定日期：2026-09-20**  
**项目名：`color-picker` · 可执行文件：`color-picker.exe`**  
**用途：作为仓库中的 `docs/implementation-plan.md`，按里程碑逐步实施。**

> 唯一技术路线：**Rust + `windows` crate + Win32 原生窗口/控件 + GDI 屏幕采样与绘制**。
>
> Windows 优先，低常驻开销优先。第一版不引入其他 GUI 框架、WebView、异步运行时或 GPU 采集管线，也不拆成多个进程。本文中的参数是项目初始设计值，不是已经测出的性能结果。代码片段用于建立工程与接口契约，不代表完整程序已经在 Windows 编译或运行通过。

## 1. 已确定的范围与默认行为

### 1.1 第一版必须完成

| 项目 | 固定要求 |
|---|---|
| 运行平台 | Windows 11 x64；Windows 10 22H2 x64 作为兼容性验证目标，不代表对其系统维护状态的推荐 |
| 发布目标 | `x86_64-pc-windows-msvc` |
| 运行方式 | 当前用户会话中的普通桌面进程，托盘常驻；不是 Windows 服务，不默认提权 |
| 激活入口 | 可配置的系统级快捷键，以及托盘“开始取色” |
| 实时预览 | 鼠标附近显示颜色、HEX、源屏幕物理像素坐标 |
| 滚轮放大 | 第一次向上滚动时冻结鼠标附近图像，再对同一缓存调整倍率 |
| 点击选择 | 处理完整左键按下/抬起；以抬起事件的坐标确认一次选择 |
| 结果窗口 | 色块、源坐标、HEX、RGB、CSS RGB、HSL；各格式独立复制 |
| 取消 | Esc 或右键完整点击；不修改剪贴板 |
| 设置 | 快捷键、默认复制格式、选择后是否自动复制 |
| 退出到后台 | 关闭结果/设置窗口回到待机；托盘“退出”才结束进程 |
| 多显示器 | 支持负坐标、混合 DPI、显示器边缘；一次冻结图像限定在一个显示器内 |
| 隐私 | 不联网、不遥测；截图只在内存中，不保存屏幕图像 |

PowerToys 的参考交互是快捷键激活、滚轮放大、点击选择；其文档明确说明滚轮放大可冻结图像。这里只复刻基础交互，不复制其完整编辑器、设置工程或代码依赖。[S03]

### 1.2 初始参数

| 参数 | 初值 | 说明 |
|---|---|---|
| 默认快捷键 | `Ctrl + Alt + C` | 注册失败必须提示冲突；不能假定此组合一定可用 |
| 预览采样间隔 | 请求 `17 ms` | 约 60 次/秒的设计上限，不保证实际 60 FPS；不提高系统计时器精度 |
| 冻结范围 | 按初始视口和 4× 倍率反推，常规至少 `65 × 65`，每轴最多 `512` 个源物理像素 | 随 DPI 增大以填满视口，靠近边缘时平移；显示器不足时缩小，不跨屏拼接 |
| 放大档位 | `4× → 8× → 16× → 32×` | 每个源像素对应 4/8/16/32 个显示物理像素 |
| 缩小到最低档以下 | 返回实时预览 | 丢弃旧冻结图像；下次放大重新截取 |
| 放大镜视口 | 每轴按 `240 DIP` 缩放，受工作区和最大缓存限制，再向下取完整 4× 像素格 | 屏幕中央与边缘同尺寸；工作区不足时允许矩形，本次冻结期间窗口与视口固定 |
| 默认格式 | 大写 HEX，例如 `#409EFF` | 结果窗口同时显示其他格式 |
| 自动复制 | 默认关闭 | 默认只弹窗；用户可在设置中开启 |
| 历史/开机启动 | 第一版不做 | 先完成低开销取色闭环 |

第一版快捷键设置支持 Ctrl/Alt/Shift 加字母、数字或 F1–F11，至少包含 Ctrl 或 Alt。主键点击后监听一个支持的按键，Esc、焦点离开或窗口失活时取消监听；修饰键仍用复选框选择。不提供 Win 组合键和复杂组合键录制。`RegisterHotKey` 有冲突和保留键限制，F12 不应注册；使用 `MOD_NOREPEAT` 抑制按键自动重复。[S05]

### 1.3 明确的边界

第一版是**普通 SDR 桌面的 8 位 RGB 取色工具**。它报告当前采集路径得到的合成颜色，不承诺恢复原文件颜色、原始 alpha、ICC 管理前数值、HDR 或广色域原始值。GDI 位块复制并不自动进行色彩管理；PowerToys 同样注明不支持 HDR/WCG 颜色格式。[S12][S03]

UAC 安全桌面、受保护视频和无法访问的桌面不属于第一版保证范围。不能通过自动提权或绕过捕获保护来“解决”。普通管理员窗口应单独实测，不把其行为与安全桌面混为一谈。

跨平台暂不实施，仅让纯 Rust 颜色、状态和坐标逻辑不依赖 Windows。

## 2. 开发顺序：按验收条件推进

不要一口气写完所有模块。前一阶段的正确性确认后，再开始下一阶段。

| 阶段 | 实施内容 | 完成后应看到什么 | 进入下一阶段的条件 |
|---|---|---|---|
| **M0 工程与纯逻辑** | 构建、manifest、颜色格式、矩形/放大映射、测试 | 可构建的最小 Windows 程序；纯函数测试通过 | DPI manifest 确实嵌入；核心模块没有 Windows 依赖 |
| **M1 常驻外壳** | 隐藏宿主窗口、消息循环、托盘、单实例、快捷键 | 后台有托盘；快捷键触发一次提示 | 待机没有定时器/钩子；重复启动不产生第二个常驻实例 |
| **M2 实时取色** | GDI 单点采样、坐标、非激活提示窗口 | 指到哪里，显示哪里的颜色和物理坐标 | 负坐标、混合 DPI、鼠标不动但画面变化均正确 |
| **M3 输入与选择** | 会话期输入线程、鼠标/键盘钩子、取消、成对释放 | 点击只取色，不点击底层程序；Esc/右键取消 | 无点击/滚轮穿透；输入线程正常结束；快速重复激活不串会话 |
| **M4 冻结放大** | 局部快照、整数放大、源像素映射 | 向上滚轮冻结；能选择单像素并显示源坐标 | 源像素与缓存值逐点一致；冻结静止时不采样、不定时重绘 |
| **M5 结果与复制** | 原生结果窗口、格式展示、剪贴板 | 点击后可复制各格式，重新取色 | 显示和复制完全一致；复制失败不伪报成功 |
| **M6 设置与恢复** | 原生设置、持久化、热键事务更新、会话/显示变化处理 | 快捷键可修改且重启保留；异常有明确恢复 | 配置损坏、热键冲突、锁屏/显示器拔插等测试通过 |
| **M7 性能与发布** | 资源回归、延迟记录、构建检查、干净机器验证 | 可日常使用的 v0.1.0 | 第 16 节测试矩阵及第 19 节清单全部通过 |

M2 可以通过托盘或临时测试按钮开始/结束，不必提前写不完整的全局输入处理。M3 先把输入闭环做对，再加入 M4 的放大逻辑。

## 3. 工程建立与依赖

### 3.1 环境

使用 Windows 上的 Rust MSVC 工具链，以及 Visual Studio Build Tools 中的 C++ 桌面构建工具和 Windows SDK。建立工程：

```powershell
cargo new color-picker --bin
cd color-picker
rustup target add x86_64-pc-windows-msvc
```

首次成功构建后，记录 `rustc -Vv`、`cargo -V` 和 Windows SDK 版本，并把实际使用的 Rust 版本写入 `rust-toolchain.toml`。不要在文档中猜测将来某天的 stable 版本。提交 `Cargo.lock`，后续构建使用 `--locked`。

### 3.2 Cargo.toml 基线

本次查询确认 `windows` 文档版本为 `0.62.2`，`embed-resource` 为 `3.0.11`。以下以这两个精确版本作为起点，而非混用不同版本的示例签名。[S01][S02]

```toml
[package]
name = "color-picker"
version = "0.1.0"
edition = "2024"
build = "build.rs"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[target.'cfg(windows)'.dependencies]
windows = { version = "=0.62.2", features = [
    "Win32_Foundation",
    "Win32_Graphics_Dwm",
    "Win32_Graphics_Gdi",
    "Win32_Security",
    "Win32_System_Com",
    "Win32_System_DataExchange",
    "Win32_System_LibraryLoader",
    "Win32_System_Memory",
    "Win32_System_Ole",
    "Win32_System_ProcessStatus",
    "Win32_System_RemoteDesktop",
    "Win32_System_Threading",
    "Win32_UI_Controls",
    "Win32_UI_HiDpi",
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_UI_Shell",
    "Win32_UI_WindowsAndMessaging",
] }

[build-dependencies]
embed-resource = "=3.0.11"

[profile.release]
opt-level = "s"
lto = "thin"
codegen-units = 1
panic = "abort"
strip = "debuginfo"
```

这份列表覆盖计划中的主要 API 域；新增 API 的 feature 必须根据锁定版本实际签名确认，只补具体需要的域。不要一次开启全部 Win32 功能。`serde` 和 `serde_json` 的实际小版本由首次生成并提交的锁文件固定。

Release 的 `panic = "abort"` 意味着不可恢复 panic 会结束进程，不执行常规析构；不能依赖 `catch_unwind` 把它变成正常清理流程。普通运行错误使用 `Result` 和显式清理，不使用 panic。[S24]

为了以原生 EXE 分发，MSVC 目标采用静态 CRT。在 `.cargo/config.toml` 中写入：

```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```

这是部署选择，不是已证明最省内存的优化。Rust 文档支持通过 `crt-static` 控制 CRT 链接方式；最终仍需检查 EXE 依赖，并在干净 Windows 环境验证。[S24b]

### 3.3 manifest 必须实际嵌入

`resources/app.manifest`：

```xml
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="color-picker"
                    version="0.1.0.0" processorArchitecture="*" />
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
                        version="6.0.0.0" processorArchitecture="*"
                        publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}" />
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
```

`resources/app.rc`：

```rc
#include <windows.h>
1 RT_MANIFEST "resources/app.manifest"
```

`build.rs`：

```rust
fn main() {
    println!("cargo:rerun-if-changed=resources/app.rc");
    println!("cargo:rerun-if-changed=resources/app.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("resources/app.rc", embed_resource::NONE)
            .manifest_required()
            .expect("failed to embed the required Windows manifest");
    }
}
```

这里按**编译目标**而不是构建脚本宿主判断 Windows。构建脚本失败必须中止，不允许静默生成缺失 DPI 声明的程序。manifest 与 DPI 默认模式参考微软文档；资源编译方法参考 `embed-resource` 文档。[S07][S07b][S02]

`src/main.rs` 顶部使用：

```rust
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
```

将业务核心导出到 `lib.rs`。非 Windows 目标仅运行纯逻辑测试，入口打印“当前仅支持 Windows”并退出；不要提供假装可用的采样后端。

## 4. 目录与职责

```text
color-picker/
├─ Cargo.toml
├─ Cargo.lock
├─ build.rs
├─ .cargo/config.toml
├─ resources/
│  ├─ app.manifest
│  └─ app.rc
├─ src/
│  ├─ main.rs                    # 参数、平台入口，不堆放业务逻辑
│  ├─ lib.rs
│  ├─ core/
│  │  ├─ mod.rs
│  │  ├─ color.rs                # Rgb8、颜色转换
│  │  ├─ format.rs               # HEX / RGB / CSS RGB / HSL
│  │  ├─ geometry.rs             # 物理坐标、矩形、裁剪
│  │  ├─ zoom.rs                 # 视口、倍率、源像素映射
│  │  └─ state.rs                # 状态、事件、转换约束
│  ├─ app/
│  │  ├─ mod.rs
│  │  ├─ controller.rs           # 状态转换与平台调用编排
│  │  ├─ config.rs               # 配置校验、加载、保存
│  │  └─ diagnostics.rs          # 按需诊断，不常驻采样日志
│  ├─ platform/
│  │  ├─ mod.rs
│  │  └─ windows/
│  │     ├─ mod.rs
│  │     ├─ host.rs              # 隐藏宿主、消息分发、单实例
│  │     ├─ tray.rs              # 托盘与 Explorer 重建
│  │     ├─ hotkey.rs            # 注册、替换、释放
│  │     ├─ input.rs             # 会话输入线程与钩子
│  │     ├─ capture.rs           # GDI 采样
│  │     ├─ monitors.rs          # 显示器、DPI、工作区
│  │     ├─ clipboard.rs
│  │     └─ resources.rs         # 类型化 RAII 封装
│  └─ ui/
│     ├─ mod.rs
│     └─ windows/
│        ├─ mod.rs
│        ├─ preview.rs           # 实时颜色/坐标提示
│        ├─ magnifier.rs         # 冻结放大镜
│        ├─ result.rs            # 原生结果窗口
│        ├─ settings.rs          # 原生设置窗口
│        └─ drawing.rs           # GDI 绘制、字体、DPI 布局
├─ tests/
│  ├─ colors.rs
│  ├─ geometry.rs
│  ├─ zoom_mapping.rs
│  ├─ state_transitions.rs
│  └─ config.rs
├─ examples/pixel-fixture.rs     # Windows 实机测试用已知像素图案
├─ scripts/verify-windows.ps1
└─ docs/
   ├─ implementation-plan.md    # 本文
   ├─ progress.md               # 已完成阶段、证据、已知问题
   └─ validation.md             # 环境、测试与资源测量结果
```

这些文件随对应阶段创建，不要先铺满只有空实现的目录。`core` 不导入 `windows`，`unsafe` 限制在平台和原生 UI 边界。第一版直接使用具体的 `GdiSampler`，不预先设计可插拔渲染器、跨平台窗口层或大型 trait 层级。

## 5. 状态机与线程模型

### 5.1 应用状态

```text
Idle
  │ 激活
  ▼
Starting ──初始化失败──────────────► Idle
  │ 资源与输入均就绪
  ▼
Live ◄────────滚轮缩到最低档以下──── Frozen
  │                                  ▲
  └────────首次向上滚轮───────────────┘
  │                                  │
  └────────有效点击 / 取消────────────┘
                     ▼
                  Finishing
                     │ 输入释放完成、钩子卸载、资源关闭
                     ├────────────► Result ──关闭──► Idle
                     └─取消/失败──► Idle

Idle ⇄ Settings
Result ──重新取色/快捷键──► Starting
```

| 状态 | 允许存在的工作 |
|---|---|
| `Idle` | 宿主窗口、托盘、快捷键和系统通知；阻塞等待消息 |
| `Starting` | 建立本次资源、启动输入线程，异步等待就绪 |
| `Live` | 有限频率单点采样、提示更新、会话输入处理 |
| `Frozen` | 读取快照、鼠标事件驱动更新；不持续抓屏 |
| `Finishing` | 不再接受新选择；收尾当前输入，停止资源 |
| `Result` / `Settings` | 原生控件按系统消息工作；没有取色钩子和采样器 |

所有异步事件携带递增 `SessionId`。旧会话的消息、定时器和线程确认不能操作新会话。`Starting`、取色期间、`Finishing` 中再次收到激活快捷键直接忽略；不会生成嵌套会话。`Settings` 打开时快捷键不开始取色，避免未提交配置和取色交叉。

### 5.2 固定线程分工

**主线程**拥有窗口、应用状态、GDI 采样与 UI 更新。待机时使用 `GetMessageW` 阻塞；返回 `-1` 是错误，`0` 是退出，不能只用布尔判断混淆它们。[S04]

**输入线程**仅在一次取色会话期间创建，安装 `WH_MOUSE_LL` 与仅处理 Esc 的 `WH_KEYBOARD_LL`，并运行自己的消息循环。两种钩子都只在本线程安装和卸载，禁止绘图、截图、配置 I/O 或剪贴板操作。微软要求低级钩子及时返回，并建议独立线程转交工作；超时可能被系统静默移除。[S08][S09]

正常收尾后，输入线程退出并回收。**应用自己创建的工作线程在待机时为零**；这不是承诺进程总线程数始终只有一个，系统组件可能有其线程。

主线程不能在输入线程仍可能收到钩子回调时直接无限 `join()`。先发停止请求，保持消息循环，收到已卸载/退出确认并确认线程结束后再回收。

### 5.3 跨线程通信

移动事件不进入无限队列：输入线程把最新坐标写入一个原子槽，以 `pending` 标志合并通知，再用 `PostMessageW` 唤醒主线程。主线程先清除 `pending` 再读取最新值，避免清除顺序造成丢失唤醒；允许偶发重复通知，不允许丢掉最终位置。

按键、滚轮等有顺序语义的离散事件，用 `std::sync::mpsc::sync_channel(64)` 和 `try_send`。不在回调中使用会等待容量的 `send`，也不 `SendMessage` 等待 UI；`try_send` 在队列满/断开时返回错误。[S25]

队列满、唤醒失败或接收端消失必须设置本次会话故障标志并进入取消/释放流程，不能继续显示“正常取色”却丢失确认事件。回调不格式化日志、不申请业务图像、不调用用户代码。不要把操作系统提供的 `MSLLHOOKSTRUCT*` / `KBDLLHOOKSTRUCT*` 指针传到另一个线程，必须在回调中复制需要的字段。

冻结状态的鼠标移动仍需合并；若重绘距离上次不足 17 ms，只安排一个短期 UI 更新定时器。处理后立刻取消，静止后没有定时重绘。它与 `Live` 采样定时器必须是不同的用途/ID。

## 6. 核心数据与接口契约

以下是接口形状，不是要求逐字复用的完整实现。以这些边界约束模块依赖即可。

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenPointPx {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRectPx {
    pub left: i32,
    pub top: i32,
    pub right: i32,   // exclusive
    pub bottom: i32,  // exclusive
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionId(pub u64);

pub struct FrozenImage {
    pub origin: ScreenPointPx,
    pub width: u32,
    pub height: u32,
    pub stride_bytes: usize,
    pub bgrx: Vec<u8>, // top-down；最后一字节无 alpha 语义
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    Live,
    Frozen,
}

pub struct PickedColor {
    pub rgb: Rgb8,
    pub source: ScreenPointPx,
    pub kind: SampleKind,
}
```

关键接口：

```text
GdiSampler::new() -> Result<GdiSampler, CaptureError>
GdiSampler::sample_pixel(point) -> Result<Rgb8, CaptureError>
GdiSampler::capture_rect(rect) -> Result<FrozenImage, CaptureError>

FrozenImage::pixel_at(x, y) -> Option<Rgb8>
ZoomView::hit_test(mouse_screen) -> Option<SourcePixel>
ZoomView::change_scale(next_scale, anchor) -> ZoomView

format_color(rgb, format) -> String
load_config() -> Result<Config, ConfigError>
save_config_transaction(...) -> Result<(), ConfigError>

InputSession::start(session_id, notify_hwnd) -> Result<InputSession, InputError>
InputSession::request_finish(reason)
Clipboard::copy_text(owner_hwnd, text) -> Result<(), ClipboardError>
```

坐标与颜色作为一个采样结果整体更新，禁止显示“新坐标 + 上一次颜色”。`CaptureError` 必须区分无有效显示器、桌面不可访问、API 失败等情况；黑色 `#000000` 是正常颜色，不可用它充当错误值。

纯逻辑矩形采用左闭右开区间。减法、面积及索引使用足够宽的中间类型和 checked 运算，坐标确认在范围内后才转换为无符号索引。

## 7. M1：后台外壳、托盘与快捷键

### 7.1 宿主窗口必须是隐藏的顶层窗口

创建一个不显示的普通顶层宿主 HWND，承载快捷键、托盘回调和系统消息。**这里不使用 `HWND_MESSAGE`**：message-only 窗口不接收广播消息，而宿主需要处理显示变化以及 Explorer 重建后的通知。输入线程内部若需要一个纯通信窗口，可以使用 message-only 窗口。[S14]

主消息循环处理至少以下消息：

| 消息/事件 | 动作 |
|---|---|
| `WM_HOTKEY` | 经状态检查后发出 Activate 事件 |
| 托盘回调 | 开始取色、打开设置、退出 |
| 自定义 `WM_APP_*` | 输入通知、线程就绪/停止、应用内部任务 |
| `WM_TIMER` | 按用途和会话验证后采样或执行单次 UI 更新 |
| `WM_DISPLAYCHANGE` | 使显示器缓存失效；取色中则取消本次会话 |
| `WM_SETTINGCHANGE` | 下次使用前刷新工作区、字体或系统设置缓存 |
| `WM_WTSSESSION_CHANGE` | 锁屏/会话断开时取消取色，不自动恢复抓屏 |
| `WM_POWERBROADCAST` | 挂起时停止会话；恢复后按下一次激活重新初始化 |
| `TaskbarCreated` | Explorer 重启后重新添加托盘图标 |
| `WM_QUERYENDSESSION` / `WM_ENDSESSION` | 允许系统结束会话；尽力有序关闭，不阻塞关机 |

会话通知通过 `WTSRegisterSessionNotification(..., NOTIFY_FOR_THIS_SESSION)` 注册，并与注销成对。注册早于相关服务就绪可能失败，需要记录状态；不能假定收到所有系统场景的通知，采集失败仍需自行处理。[S21]

### 7.2 托盘与单实例

托盘使用 `Shell_NotifyIconW`，添加后设置适当的通知图标版本；处理 Explorer 重建和退出时删除图标。菜单仅包含“开始取色”“设置”“退出”。[S20]

以当前用户/会话范围的具名互斥对象实现单实例。第二次启动找到已有宿主并发出激活请求，然后退出，不启动第二套钩子。宿主尚在启动时要有短暂、有限的重试或“已在运行”提示，不能无限等待。

第一阶段可以使用系统共享图标。对共享图标不调用销毁函数；最终应用图标随资源编译加入，不在运行时联网加载。

### 7.3 快捷键更新必须可回滚

只在宿主线程调用 `RegisterHotKey` / `UnregisterHotKey`。修改热键时不先删除旧注册：

```text
校验新配置；若热键未变化则复用旧注册
    ↓
用不同临时 ID 注册新热键，旧热键保留
    ↓ 失败：提示冲突，维持旧配置
保存新配置
    ↓ 失败：注销临时注册，维持旧配置
提交内存配置并注销旧热键
```

更换完成前不响应临时 ID；设置窗口打开时不开始取色。热键注册和配置保存都成功才在 UI 显示“已应用”。不要在冲突后偷偷换成另一个热键。

## 8. M2：物理坐标、采样与提示窗口

### 8.1 坐标统一约定

进程在创建窗口前就具备 Per-Monitor V2 DPI 模式。用物理像素处理屏幕位置和采样矩形；窗口布局单独处理 DIP 到像素转换。微软文档明确说明 PMv2 让程序看到各显示器原始像素，但原生界面仍需响应 DPI 变化。[S06]

实现 `monitors.rs`：使用 `EnumDisplayMonitors` / `GetMonitorInfoW` 获得各显示器区域与工作区；采样裁剪用显示器区域 `rcMonitor`，弹窗定位用工作区 `rcWork`，不要混淆。主屏原点为 `(0, 0)`，左侧/上侧副屏可以出现负坐标。不要用无符号类型保存屏幕位置。

源坐标保持屏幕物理像素，不因窗口 DPI 改变而缩放。布局使用 `GetDpiForWindow`，响应 `WM_DPICHANGED` 更新窗口、控件和字体。采样线程和输入线程都保持预期 DPI 上下文，不在未知 DPI 模式的线程上混用坐标。

`GetCursorPos` 用于实时预览；点击/滚轮选择优先使用对应钩子事件携带的坐标，而不是处理事件时再次读取已经移动的鼠标。使用锁定版 API 的正确结构与坐标语义。[S28]

### 8.2 GDI 采样器

`Live` 会话建立一个屏幕 DC、一个兼容内存 DC，以及可复用的 **1×1、32 位、top-down DIBSection**。top-down 使用负 `biHeight`；`biPlanes = 1`、`biBitCount = 32`、`biCompression = BI_RGB`。按 B、G、R、X 读取字节，不把 X 当成有效透明度。[S10][S11]

正常采样顺序：

```text
取得当前物理坐标并确认位于某显示器
    ↓
处理自有浮层遮挡（见 8.4）
    ↓
BitBlt 到复用的 1×1 DIBSection
    ↓
GdiFlush，确认 CPU 可以读取结果
    ↓
读取 BGR 字节，组成 {坐标, Rgb8} 一个结果
    ↓
只有可见内容变化时才触发对应重绘
```

屏幕复制使用 `SRCCOPY | CAPTUREBLT`，使正常可采集的 layered 窗口参与结果；检查 API 返回值。`BitBlt` 不替调用者处理所有源矩形裁剪，所以先自行验证和裁剪范围。[S12]

`GdiFlush` 与 `DwmFlush` 不是一回事：前者用于本线程 GDI 操作/位图访问同步，后者用于需要时与桌面合成显示同步，不能相互替代。[S10][S16]

采样器只在创建、尺寸变化或失效重建时申请图像资源，不每个 tick 创建位图/DC。`GetDC` 得到的屏幕 DC 用 `ReleaseDC`，内存 DC 用 `DeleteDC`，两者不能混用；DC 不跨线程使用。[S17]

### 8.3 实时采样节奏

只在 `Live` 安装请求间隔为 17 ms 的 `SetTimer`。移动事件可以更新最新位置，但不额外绕过限频无限截图。点击确认允许一次优先采样，不必等待下一个预览 tick。

鼠标不动时仍需采样，因为画面可能是视频/动画；但像素、坐标都没变时，不重新格式化文本、不重绘、不重新创建字体/画刷。

普通 Win32 定时器不是精确帧调度器。这里不调用 `timeBeginPeriod`、不忙等、不追补延迟帧。取消后即 `KillTimer`；已经排队的 `WM_TIMER` 仍可能到达，必须同时检查状态、有效 timer ID 和会话代际。[S19][S19b]

采样失败不把旧颜色包装成新值。瞬时失败显示“暂不可采样”，禁止确认；连续失败或显示/会话失效则停止本次会话并提示一次，不在屏幕上反复弹错误框。

### 8.4 防止采到自己的窗口

提示和放大镜采用自有非激活顶层窗口，设置捕获排除作为辅助：

```text
SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)
```

这一值从 Windows 10 2004 开始支持，调用成功也不能替代对实际 GDI 路径的验证。不能把失败退化成 `WDA_MONITOR` 并误以为一定能读到窗口背后的内容。[S15]

**默认正确性路径不依赖捕获排除单独成立：**

- 实时提示位于采样点旁边，不覆盖采样点。每次采样先与自有浮层现有矩形比较；鼠标快速进入旧提示区域时，先隐藏相交浮层，按需要进行一次 `DwmFlush`，再采样和重新摆放。
- 首次冻结前隐藏自有提示窗口，等待必要的合成同步，取得局部快照后才显示放大镜。冻结图像中不能含有本次放大镜。
- 不每帧无条件隐藏/显示窗口，不用固定 `Sleep(20)` 猜测屏幕完成时机，不把纯黑颜色判定为自污染。

`DwmFlush` 是有等待成本的同步操作，仅用于这些转换/遮挡场景，不放进输入钩子或普通每帧路径；其具体效果要用测试图案检查。[S16]

### 8.5 提示窗口与绘制

提示/放大镜使用 `WS_POPUP`，扩展样式采用 `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT`，显示时不激活，设置置顶。使用 `SetLayeredWindowAttributes` 设置整体不透明，并保留正常 GDI `WM_PAINT` 路径。第一版不混用 `UpdateLayeredWindow`。

这里的鼠标穿透建立在 layered 窗口与透明扩展样式的组合上，不是简单地认为任意窗口加 `WS_EX_TRANSPARENT` 就能跨进程穿透。鼠标动作是否被底层应用收到，由会话期钩子另行决定。[S14]

在内存后备表面绘制背景、色块、坐标与文本后一次提交，避免闪烁；表面只在窗口尺寸/DPI 变化时重建。每次 `BeginPaint` 对应 `EndPaint`，没有新内容就不主动反复 `InvalidateRect`。不修改系统全局光标；默认保留正常鼠标箭头。

信息背景采用局部软件磨砂：在捕获排除成功时，仅截取信息条背后的区域，模糊后混入约 65% 深色，保留约 35% 背景。
缓存按屏幕位置复用，移动 / DPI 或尺寸改变才更新；静止和冻结时不跟随后方动画。
效果失败回退纯色，不改变取色数据。下、右各有 2 DIP 边线（175% 缩放时为 4 个物理像素），不扩充边距。

上述数值可在设置页的两个外观滑块中调整：边框 0–6 DIP，背景透明度 0–80%，
默认 2 DIP / 35%。0 DIP 关闭边框，0% 使用纯色背景。仅“应用”后保存；
下一次取色及其 Live / Frozen 转换共用已接受的外观设置。旧配置自动补默认值。

## 9. M3：输入接管与安全结束

### 9.1 只接管必要输入

| 输入 | 取色期间行为 |
|---|---|
| 鼠标移动 | 传给后续钩子；更新最新位置，不阻止底层 hover |
| 垂直滚轮 | 消费；调整放大档位，不能让底层网页滚动 |
| 左键 | 消费完整按下/抬起；抬起后发送一次选择候选 |
| 右键 | 消费完整按下/抬起；取消 |
| 中键、侧键 | 取色期间消费完整按下/抬起，但不赋予新功能 |
| Esc | 消费按下、重复和抬起；只触发一次取消 |
| 其他键 | 原样传递，不记录输入内容 |

对于 `nCode < 0`，立刻调用后续钩子；未处理的事件也正常传递。回调对系统结构的访问必须在有效范围内。[S08][S09]

滚轮 delta 累积到完整刻度再切换档位，保留不足一个刻度的余量。每次改变倍率只处理合理数量的档位变化，不把高频滚轮事件转成无限重绘任务。

安装前检查鼠标按钮是否已按住。若正在拖动，拒绝本次启动并提示“请先松开鼠标”，不要在已有手势中途开始吞事件。激活热键的 Ctrl/Alt 等释放事件仍正常传递。

### 9.2 输入状态必须独立于应用业务状态

输入线程维护自己的 `Capturing / AwaitDecision / Draining / Stopped` 状态以及被本程序吞掉的按钮/键的按下集合。

```text
Capturing
   │ 左键完整释放，发出候选
   ▼
AwaitDecision
   ├─主线程：位置无效────────► Capturing
   └─主线程：选中/取消/错误──► Draining
                                  │ 被消费手势都已释放
                                  ▼
                               Stopped
```

候选发出后不能继续产生第二次选择。等待主线程决定期间仍处理必要的释放事件，主线程不通过同步调用卡住回调。冻结模式点击窗口外取消；窗口内边框、信息栏或无效留白不成立为候选：继续取色，不“夹到最近像素”并悄悄选择。

取消时也不能只卸载钩子：若已经吞掉 Esc-down 或鼠标 down，应尽量把对应 up 同样处理完。普通退出路径必须保证没有半个点击落到下面的应用；不通过 `SendInput` 补发到未知窗口。

### 9.3 结束顺序

```text
停止产生新的采样和选择
    ↓
保存有效的 PickedColor（或者取消原因）
    ↓
通知输入线程进入 Draining
    ↓
等待已消费手势完整释放；主线程继续泵消息
    ↓
输入线程卸载两种钩子并退出
    ↓
确认线程已退出，回收线程上下文与通信资源
    ↓
销毁取色浮层、采样器、快照及本次 timer
    ↓
展示结果，或直接回到 Idle
```

卸载钩子和回收回调上下文的时序必须正确；不能因为 `UnhookWindowsHookEx` 返回就任意释放另一个仍在执行的回调需要的数据。[S27]

异常情况采用**不长期锁住输入**的原则：队列断开、主窗口销毁或桌面切换时停止新拦截并尽力清理；`Draining` 可以设置一次性 5 秒故障超时，超时强制退出会话并标记错误。这个超时是异常保险，不用于提前完成正常点击。进程崩溃、桌面切换等异常路径不能承诺完整配对，不能把它们算作“正常路径无穿透”已经通过。

### 9.4 点击时刻的诚实边界

`Live` 以左键抬起事件坐标尽快采样，不使用过期预览缓存冒充点击结果。但 UI 线程实际截图晚于输入事件发生，不保证捕捉视频中严格同一时刻的那一帧。

`Frozen` 则直接读取被冻结的源图像，得到确定的像素值。需要对动态画面精确选择时，应使用冻结模式；第一版不为“严格事件时间帧”引入持续全屏录制。

## 10. M4：冻结放大与源像素映射

### 10.1 首次冻结

在主线程按顺序完成：

```text
验证滚轮事件属于当前 Live 会话
    ↓
取事件位置 P 与所在显示器 M
    ↓
停止 Live 定时器，隐藏自有提示，必要时同步桌面合成
    ↓
创建隐藏的放大镜窗口取得目标 DPI，先确定标准视口与窗口位置
    ↓
按鼠标所在像素格反推初始源视图，规划 M.rcMonitor 内与 DPI 视口匹配的快照（每轴最多 512）
    ↓
GDI 一次截取该局部区域，复制为拥有自身内存的 FrozenImage
    ↓
初始化鼠标锚定视图与 4× 倍率，显示放大镜
    ↓
释放 Live 的采样 DC/位图，不再保留持续采样器
    ↓
进入 Frozen，刷新当前鼠标悬停状态
```

快照矩形限定在当前显示器内，不把虚拟桌面的外接矩形当成每个像素都有效的图像。这样显示器间空隙、混合 DPI 边界和不同显示设备不会悄悄产生“补黑可选像素”。

2026-09-22 的边缘对齐与高 DPI 版本先布局再截图：按视口物理尺寸除以 4 确定缓存范围，靠近边缘时平移，仅显示器不足时缩小。初始源视图起点为 `P - floor((P - D.origin) / 4)`，捕获区域需包含该视图；初始化时保留原始像素锚点。保存真实 `origin/width/height`，原始鼠标点在缓存中的位置由 `P - origin` 得出，不假定永远在 `(32, 32)`。鼠标落在信息栏、边框或工作区外时不承诺原地命中。

### 10.2 放大镜视口规则

本次会话建立固定窗口和视口位置，尽量以首次滚轮位置为图像中心，整体限制在当前显示器工作区内。视口每轴按 240 DIP 随系统缩放，受工作区和最大 512 源像素缓存限制；可用视口小于 32 物理像素时拒绝冻结。175% 下视口为 420×420 物理像素，初始 4× 对应 105×105 快照。视口无外围边距，底部保留 28 DIP 的单行颜色、坐标与倍率信息栏，字体随 DPI 缩放。实时预览为 168×38 DIP，色块贴边，操作说明统一放在设置页。

冻结模式下，左键点击窗口外部取消取色，消费该次完整点击后释放输入钩子，不生成结果；窗口内边框、留白与信息栏不取色，保持等待。

倍率就是整数物理像素比例，不再乘一次 DPI。DPI 只控制文字、边距和视口布局。例如 8× 表示一个源物理像素绘制成 8×8 个显示物理像素。

视口显示缓存的一个整数像素子矩形 `source_view`。源图像小于视口时居中并留白；放大后只显示能完整放下的像素格，不伸出窗口、不把整个窗口放大到 65×32 像素宽。

滚轮调倍率时，优先保持鼠标当前指向的**缓存像素**在鼠标下；鼠标在无效区域时以此前有效选中像素为锚点。视图原点只做整数平移，边缘以缓存范围约束。窗口本身不追着鼠标移动。本次视口、缓存和显示器布局信息一并存入 `ZoomView`。

### 10.3 映射公式

定义：

```text
P = 鼠标的屏幕物理坐标
D = 实际绘制有效像素格的屏幕矩形（不含边框、文字栏和留白）
k = 每个源像素的显示宽度/高度，取 4、8、16、32
V = 当前 source_view 在 FrozenImage 内的左上角
O = FrozenImage 对应的原屏幕左上角
```

当 `P` 在 `D` 内时：

```text
cache_x = V.x + floor((P.x - D.left) / k)
cache_y = V.y + floor((P.y - D.top) / k)

source_x = O.x + cache_x
source_y = O.y + cache_y

color = FrozenImage.pixel_at(cache_x, cache_y)
```

必须先检查 D 的左闭右开边界，再做除法；不能让负数整数除法向零截断把左侧越界点误判为第一个像素。实际缓存范围还需再检查一次。

界面显示 `source_x/source_y`，而不是放大镜窗口下面的鼠标位置。索引计算、UI 提示、确认选择必须调用同一个 `hit_test`，不能各写一份近似公式。

### 10.4 绘制与数据读取彻底分开

从不可变的 top-down BGRX 缓存使用 `StretchDIBits` 绘制整数倍像素，目标 DC 设置 `COLORONCOLOR`，不使用平滑插值。格子边框、选中框、文本在绘图层叠加，颜色只从未叠加的缓存读取。[S13][S13b]

纵向裁剪先借用缓存中需要的完整行切片，DIB 保留原图行宽，负高度改为可见行数，
`ySrc` 固定为 0；横向仍由 `xSrc` 裁剪。这样实际绘制的行与从顶部计算的命中坐标一致，
不依赖非零 `ySrc` 的纵向裁剪约定，不增加复制或重新截图。

较高倍率显示细网格；选中像素用内外两层对比边框标记，避免只用一种颜色而在同色背景上看不见。不要为了加高亮去改 `FrozenImage.bgrx`。

静止的 `Frozen` 不采样屏幕，也不运行周期性重绘。向下滚出最低档时销毁快照/放大镜，重新创建 Live 采样器并恢复实时提示。

## 11. M5：结果窗口、格式和复制

结果窗口用标准 Win32 原生 `STATIC`、只读 `EDIT`、`BUTTON` 控件。色块用简单自绘。文本可选中，按钮有 Tab 顺序与键盘操作，不用一整块无可访问语义的自绘文字代替所有控件。

初始布局：

```text
color-picker

[ 色块 ]  X: -120   Y: 846
          来源：实时 / 冻结

HEX      #409EFF                     [复制]
RGB      64, 158, 255                 [复制]
CSS RGB  rgb(64 158 255)              [复制]
HSL      hsl(...)                    [复制]

[复制默认格式]       [重新取色]       [关闭]
```

窗口默认不永久置顶；取色结束时请求显示并聚焦，系统不允许抢焦点时保持正常可见，不使用不断切换前台的循环。结果窗口关闭后销毁，而不长期隐藏保留整套控件。

### 11.1 格式定义

| 格式 | 输出规则 |
|---|---|
| HEX | `#RRGGBB`，字母大写、固定两位 |
| RGB | 十进制 `r, g, b` |
| CSS RGB | `rgb(r g b)`，使用空格分隔 |
| HSL | `hsl(h s% l%)`，内部使用浮点，显示最多一位小数并去掉冗余 `.0` |

颜色计算以 `Rgb8` 为唯一来源，界面与剪贴板调用同一个格式化函数。灰度 HSL 的 hue 统一为 0，饱和度为 0，防止除零；色相取 `[0, 360)`，舍入后出现 360 时归一为 0。CSS 语法与 HSL 定义核对 W3C 文档。[S26]

第一版不显示“原始 alpha”。结果来自合成屏幕，不能唯一恢复参与合成前的透明度。

### 11.2 剪贴板

固定使用 Win32 `CF_UNICODETEXT`，以真实宿主/结果 HWND 作为 owner。该常量在当前 `windows` 绑定的 `Win32::System::Ole` 下，因此 Cargo 基线包含这个 feature。[S29]

复制流程：

```text
准备以 NUL 结尾的 UTF-16 文本和 GMEM_MOVEABLE 内存
    ↓
OpenClipboard(owner_hwnd)
    ↓
EmptyClipboard
    ↓
SetClipboardData(CF_UNICODETEXT, handle)
    ↓
CloseClipboard
```

`SetClipboardData` 成功后内存所有权转移给系统，本程序不再释放；失败时由本程序释放。`OpenClipboard` 失败则不要继续 Empty/Set，也不声称“已复制”。不能用空 owner 开剪贴板再假定所有后续所有权操作正常。[S18][S18b]

剪贴板被占用时通过 UI 定时器进行最多 3 次、间隔 50 ms 的短暂重试；它属于正在执行的复制操作，结束即取消，不在待机周期运行。窗口关闭或出现新的复制请求时，旧重试 token 失效。

复制成功在窗口内显示状态，不另外弹模态框。自动复制打开时，在取色资源清理完毕后执行；复制失败不丢弃已经得到的颜色结果。

## 12. M6：设置、持久化与恢复

### 12.1 配置结构

默认位置：`%LOCALAPPDATA%\color-picker\config.json`。使用 Windows Known Folder 获取实际目录，避免假设用户路径、用户名或固定盘符。

```json
{
  "schema_version": 1,
  "hotkey": {
    "ctrl": true,
    "alt": true,
    "shift": false,
    "key": "C"
  },
  "default_format": "hex",
  "auto_copy_on_pick": false,
  "appearance": {
    "border_width_dip": 2,
    "background_transparency_percent": 35
  }
}
```

实时采样间隔、快照尺寸和倍率在第一版保留为代码常量，不暴露大量会破坏体验/性能的设置。配置 UI 提供三个修饰键复选框、点击后监听的主键按钮、默认格式下拉框和自动复制复选框、边框粗细和背景透明度滑块，以及简短取色操作说明。旧配置缺少 appearance 或其中字段时自动补默认值；外观数值越界时保留原文件并禁止保存。

加载时校验 schema、枚举值、组合键与类型。配置缺失使用默认值；损坏时保留原文件并提示一次，不能静默覆盖用户数据。未来 schema 高于当前支持版本时停止改写该文件并使用明确的恢复策略。

写入使用同目录临时文件，完整写入并 `sync_all` 后关闭文件，再用 `std::fs::rename` 替换目标；失败保留旧文件，不先删除旧配置。替换失败必须处理，不能假定权限或文件系统总允许操作。[S30]设置只有点击“应用”时保存，无配置轮询和常驻文件监视。

### 12.2 异常恢复原则

| 情况 | 固定处理 |
|---|---|
| 热键注册冲突 | 保留旧值或待机入口，设置中说明，不自动另选组合 |
| GDI/输入初始化失败 | 按已创建资源逆序释放，回 Idle，提示一次 |
| 显示拓扑/分辨率改变 | 当前冻结图像和映射失效，取消会话；下次激活重建 |
| 仅结果窗口跨 DPI | 重新布局结果控件与字体，不改变已选颜色/源坐标 |
| 锁屏、会话断开、挂起 | 停止取色；恢复后不自动抓屏 |
| Explorer 重启 | 恢复托盘，不重复注册第二个热键或创建第二个输入线程 |
| 剪贴板忙 | 有限重试，失败保留结果 |
| 配置保存失败 | 维持旧配置、旧快捷键和可重试设置界面 |
| 自有窗口捕获排除失败 | 使用明确的遮挡处理路径，不把错误解释成有效颜色 |

无法识别的受保护/异常画面有时也会表现为有效黑色像素。不得用颜色值猜测保护状态，更不能把保护内容一定可检测作为功能保证。

## 13. 资源生命周期与 Rust 安全边界

### 13.1 资源所属状态

| 资源 | 生存期 | 清理要求 |
|---|---|---|
| 单实例对象、宿主、托盘、热键 | 进程 | 正常退出显式关闭 |
| 输入线程与两种钩子 | Starting 到 Finishing | 在线程退出前卸载钩子，确认结束再回收上下文 |
| 1×1 采样 DC/DIB | Live | 离开 Live 即释放；冻结转换临时截图完成后释放 |
| 冻结像素 Vec | Frozen | 返回 Live、确认或取消时释放 |
| 提示/放大镜 HWND 和后备表面 | 对应取色状态 | 离开后销毁，不能只 Hide |
| 结果/设置 HWND、字体、控件资源 | 窗口打开期间 | 关闭即销毁 |
| 定时器 | 对应具体工作 | 停止同时让旧消息失效 |

### 13.2 类型化 RAII

分别封装 `ScreenDc`、`MemoryDc`、`OwnedBitmap`、`OwnedFont`、`HookGuard`、`HotkeyGuard` 和剪贴板操作。不要编写“所有句柄统一 CloseHandle”的通用销毁器。

GDI 对象删除前必须先从 DC 中选出，恢复 `SelectObject` 返回的旧对象；否则删除可能失败。位图选择恢复、位图删除、DC 删除的顺序写进封装，不交给调用者记忆。[S17][S17b]

GDI sampler 固定在主线程，用类型或私有构造限制跨线程移动，不给原始句柄随意实现 `Send/Sync`。共享系统 stock 对象不当作自有资源销毁。

### 13.3 FFI、重入与销毁

Win32 创建、显示、销毁窗口可能同步触发窗口过程。因此不能持有 `&mut AppState` 或 `RefCell` 的可变借用，跨越可能重入窗口过程的 API 调用。

主线程先完成一个短状态转换，结束借用，再执行平台/UI 副作用。把 HWND 对应数据指针的建立、读取、`WM_NCDESTROY` 清空与最终释放集中实现，防止 `Box::from_raw` 调用两次或销毁过程访问悬空指针。

所有 `extern "system"` 回调禁止让 Rust unwind 穿过 FFI。开发构建需要明确的边界错误处理；Release 的不可恢复 panic 选择结束进程而不是继续使用可能损坏的全局输入状态。普通 API 失败都走正常错误路径。

## 14. 占用验收：先消除无效工作，再优化数字

### 14.1 必须满足的结构性指标

| 场景 | 取色定时器 | 输入钩子 | 屏幕采样 | 主动重绘 |
|---|---:|---:|---:|---|
| Idle | 0 | 0 | 0 | 无应用周期性重绘 |
| Live | 1 个限频采样定时器 | 会话期鼠标 + Esc 钩子 | 每 tick 最多 1 次预览；确认/转换可额外一次 | 内容改变才刷新 |
| Frozen 静止 | 0 | 同上 | 0 | 0 次应用周期性重绘 |
| Result / Settings 静止 | 0 | 0 | 0 | 仅系统/用户需要的重绘 |

短暂的 UI 合并、复制重试、异常收尾定时器不能泄漏到 Idle。无网络连接、无心跳、无后台日志刷新、无自动检查更新。不要调用清空工作集之类的方法只让任务管理器数字看起来更小。

### 14.2 测量方法

分别测“刚启动待机”和“实际使用后待机”，不能只报告最漂亮的冷启动数值。记录以下场景，各场景在同一机器、相同构建与显示配置下重复：

| 场景 | 方法 |
|---|---|
| 冷启动待机 | 启动后记录基线，再静置 60 秒 |
| 使用后待机 | 取色、放大、复制、关闭，静置 60 秒 |
| Live 动态预览 | 固定时长移动鼠标并显示动态画面 |
| Frozen 静止 | 冻结后鼠标不动，静置 60 秒 |
| 会话循环 | 先预热 20 次，再分别测 100/500/1000 次开始、取消、结果关闭 |
| 热唤起延迟 | 热键事件被处理至提示首帧提交，记录 p50/p95 |
| 冻结延迟 | 首次滚轮事件处理至放大镜首帧提交，记录 p50/p95 |

记录私有提交、工作集、进程 CPU 时间增量、线程数、句柄数、GDI/USER 对象数，以及本程序计数器中的有效 timer/hook/session 数。`PrivateUsage` 与工作集不是同一个概念，`GetGuiResources` 可检查 GDI/USER 对象。[S22][S23]

需要观察实际屏幕呈现延迟时另做录屏/显示测试；不能把“提交首帧”直接当成用户已看见的精确时间。

初始工程预算：在主力开发机上争取热唤起 p95 不超过 100 ms；若未达到，记录初始化、采集、自污染处理和绘制各段耗时再优化。这个预算不是跨设备保证。内存暂不写“必须低于某 MB”的虚构数字，先建立基线；预热后重复会话不能呈现随次数线性增长的资源趋势。

只在诊断模式记录事件级耗时，并限制日志容量。默认运行不输出每个采样 tick 的日志。

## 15. 自动测试与 Windows 实机验证

### 15.1 纯逻辑测试

| 测试组 | 必须覆盖 |
|---|---|
| 颜色格式 | 黑、白、红、绿、蓝、灰；HEX 补零；RGB/CSS 的固定空格；HSL 灰度与舍入 |
| 矩形 | 负坐标、空交集、右/下边界不包含、极端输入不溢出 |
| 快照访问 | BGR/RGB 顺序、top-down 行序、stride、索引越界返回 None |
| 放大命中 | 每个倍率、每个格边界、负屏幕原点、文字栏/留白无效 |
| 缩放锚点 | 改倍率前后保持同一源像素；缓存边缘约束正确 |
| 状态 | 启动失败回滚、取消、结果重取、重复热键、旧 SessionId 被丢弃 |
| 输入协议 | down/up 配对、Esc 重复、AwaitDecision、队列故障、Draining |
| 配置 | 默认值、损坏 JSON、未知枚举、schema 不支持、保存失败回滚 |

为 `zoom_mapping` 构造每个像素具有确定颜色和坐标的缓存，遍历每个有效源像素及倍率做往返验证。界面能显示不代表映射正确，必须测到格边界和窗口裁剪。

### 15.2 已知像素测试程序

建立 `examples/pixel-fixture.rs`，同样使用 PMv2，在客户区绘制无抗锯齿的单像素图案。例如：

```text
R = x mod 256
G = y mod 256
B = (x XOR y) mod 256
```

这里 x/y 是测试图案内部非负整数坐标；程序同时显示它在屏幕上的物理原点。取色器应返回对应位置精确的 RGB。另加 1 像素红绿蓝条纹，用于发现通道交换、缩放插值和错位。

对这项逐字节精确验收，使用受控 SDR 环境，关闭会改变显示输出的滤镜，并记录色彩设置。不要用浏览器中经过页面缩放/图像插值的图片作为唯一真值。

### 15.3 构建验证

`scripts/verify-windows.ps1` 按顺序执行以下检查，每一步非零退出码立刻失败：

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked --target x86_64-pc-windows-msvc
```

注意 PowerShell 的 `$ErrorActionPreference = 'Stop'` 本身不能替代所有外部进程的 `$LASTEXITCODE` 检查，脚本应在每次调用后明确检查。

单元测试不要依赖用户当前剪贴板、当前桌面颜色或安装真实全局钩子。需要交互桌面的集成测试标记为单独运行，在可控环境中执行并恢复环境。

Windows CI 负责原生构建、Clippy 和纯测试；其他平台仅执行核心逻辑测试。无交互桌面的 CI 成功不能代替鼠标输入、DPI、捕获排除和权限测试。

## 16. v0.1 发布前实机测试矩阵

| 编号 | 场景 | 通过标准 |
|---|---|---|
| W01 | 单屏 100% 缩放 | 坐标与测试图案逐像素一致 |
| W02 | 125%、150%、200% | 不出现双倍缩放/逻辑坐标混用 |
| W03 | 混合 DPI 双屏反复跨越 | 颜色准确；文字尺寸正确；窗口不漂移 |
| W04 | 副屏位于主屏左侧/上侧 | 负坐标正确，无整数溢出 |
| W05 | 显示器之间有空隙 | 不把空隙当作可选黑色像素 |
| W06 | 屏幕四角、边缘和任务栏附近 | 截图正确裁剪；弹窗不越出可见工作区 |
| W07 | 浏览器按钮、滚动页、可拖动窗口下取色 | 点击不执行底层动作，滚轮不滚动底层页面 |
| W08 | 冻结后底层画面继续变化 | 选取始终来自原缓存；源坐标保持正确 |
| W09 | 光标快速越过旧提示窗口 | 不采到自己的色块/文字 |
| W10 | 每档放大、缩小、点击格边界 | 选中格与颜色、源坐标完全一致 |
| W11 | 鼠标在放大镜外/文字栏/留白处点击 | 窗外取消且不穿透；窗口内无效区域不误选、继续等待 |
| W12 | 按住鼠标启动、长按、双击、Esc 连按 | 无半个手势传给底层，无重复结果窗口 |
| W13 | 快速启动/取消/再次启动 | 旧消息不会关闭新会话或显示旧颜色 |
| W14 | 热键冲突、保存失败、损坏配置 | 旧状态不被破坏，有清晰提示 |
| W15 | 剪贴板短暂占用 | 有限重试；结果不丢；不伪报成功 |
| W16 | Explorer 重启 | 托盘恢复；无重复实例和重复热键 |
| W17 | 取色中锁屏/解锁、睡眠/唤醒 | 自动停止；恢复后不自动抓屏 |
| W18 | 拔插显示器、切分辨率、远程桌面断开 | 不继续使用旧映射/旧 DC；下一次取色可重新初始化 |
| W19 | 普通程序、管理员程序、系统界面分别测试 | 明确记录覆盖范围和限制，不把未测场景标为支持 |
| W20 | HDR 或系统颜色滤镜开启 | 不宣称原始颜色准确；限制明确可见 |
| W21 | 连续 1000 次会话及窗口打开关闭 | 自有句柄、GDI/USER 对象和内存无持续线性增长 |
| W22 | 干净 Windows 机器 | EXE 能运行；manifest 正确；不需要开发工具才能启动 |

每项在 `docs/validation.md` 记录环境、日期、构建提交和 PASS/FAIL/NOT TESTED。未执行不填 PASS。失败项应回到对应阶段修复，不靠删除测试或隐藏错误跳过。

## 17. 发布与后续功能顺序

第一版产物为 `color-picker.exe`，通过 ZIP 分发，附 README、许可信息和已知限制。先在干净机器检查依赖、启动、退出与配置路径；不引入安装服务、后台更新器和自动提权。若实际复用了 PowerToys 代码，必须另行保留相关许可/版权信息；仅参照交互不代表复制了其源码。

后续仍沿用本方案，按这个顺序扩展：

| 版本阶段 | 功能 | 不得破坏的约束 |
|---|---|---|
| v0.2 | 最近 20 个颜色、清空历史、HSV 格式 | 只持久化颜色/格式等小数据，不保存截图；不常驻数据库 |
| v0.3 | 用户主动开启的开机启动、设置导入导出 | 关闭选项能完整移除启动项；不改变当前用户权限模型 |
| v0.4 | 键盘逐像素选择、复制动作优化 | 输入只在取色期间接管；扩展键也完整处理按下/抬起 |
| v0.5 | Windows ARM64 构建与独立实机验收 | 先验证指针/坐标打包、资源和依赖，不仅是增加一个 CI target |

macOS/Linux、HDR 精准颜色和复杂颜色编辑不放进这条首版实施链。将来确有需求再写新设计文档；当前不为它们提前引入另一套框架。

## 18. 开发记录与继续实施方法

每完成一个里程碑，在 `docs/progress.md` 写明：实现了什么、涉及哪些文件、跑了哪些命令、实机验证结果、仍未完成的验收项。只按真实结果勾选，不以“代码已写”替代“功能已验证”。

单次提交尽量只对应一个可验证目标。不要把输入接管、UI 美化、配置格式和依赖升级混在同一个大提交中。新增功能先补纯逻辑测试，再接平台行为，最后补实际输入/显示验证。

交给后续开发者或编程助手的入口文本可以直接使用：

```text
项目名为 color-picker。先阅读 docs/implementation-plan.md 和
已有的 docs/progress.md，再检查仓库当前实现。

技术栈固定为 Rust + windows crate + Win32 原生窗口/控件 + GDI。
不要重新做技术选型，不引入其他 GUI 框架、WebView、异步运行时或
GPU 采集管线，不把单进程改为多个进程。

从第一个尚未通过验收的里程碑开始，保留已完成且正确的功能。
优先保证物理像素坐标、输入按下/抬起配对、截图不含自身浮层、
状态/会话隔离和正常退出后的资源释放。

实现后执行当前环境支持的格式、Clippy、测试和构建检查；
Windows 专属场景按文档验证。无法执行的测试明确写 NOT TESTED，
不要推测通过。更新 progress.md 的完成情况和剩余验收项。

所有与本文不一致的必要改动写明原因和影响，不静默更换架构。
```

## 19. 最终完成清单

- [ ] 项目/可执行文件名统一为 `color-picker` / `color-picker.exe`。
- [ ] M0–M7 均有实现记录和验收证据。
- [ ] 主宿主是可接收广播的隐藏顶层窗口，PMv2 manifest 实际生效。
- [ ] 待机没有取色 timer、输入钩子、应用工作线程、屏幕采样或网络请求。
- [ ] 快捷键可修改，冲突和保存失败可回滚。
- [ ] 实时颜色与显示的物理坐标来自同一次有效采样。
- [ ] 单次冻结只截局部图像，放大和取色均读取该缓存。
- [ ] 放大模式显示原屏幕源坐标，不显示错误的放大窗口坐标。
- [ ] 所有正常确认/取消手势完整消费，不点击或滚动底层应用。
- [ ] 回调没有同步等 UI、截图、复制或磁盘 I/O。
- [ ] 所有 Win32 资源按正确类型/线程/顺序释放，旧会话消息不能作用于新会话。
- [ ] HEX/RGB/CSS/HSL 展示与复制一致，剪贴板失败有真实反馈。
- [ ] 混合 DPI、负坐标、边缘、自污染和 1000 次资源回归通过。
- [ ] 编译、Clippy、单元测试通过；未实测场景明确标注。
- [ ] 文档没有把未知内存占用、HDR 精度或系统权限覆盖写成已经保证。

**完成上述清单后，才开始增加历史记录、开机启动和其他附加功能。**

## 20. API 与行为依据

以下为本次核对的一手资料。模块划分、默认参数、开发顺序和性能预算属于本项目设计，不是官方提供的现成实现或基准成绩。版本可变的依赖已在第 3 节固定起点，后续升级需要重新验证。

| 引用 | 资料与用途 |
|---|---|
| [S01] | Rust for Windows 0.62.2；版本、feature 与绑定边界 |
| [S02] | embed-resource 3.0.11；构建期资源与必需 manifest 嵌入 |
| [S03] | PowerToys Color Picker；冻结放大交互及 HDR/WCG 限制 |
| [S04] | GetMessage；阻塞消息循环和返回值 |
| [S05] | RegisterHotKey；注册、冲突、MOD_NOREPEAT 与保留键 |
| [S06] | High DPI Desktop Application Development；PMv2、物理像素与原生窗口布局 |
| [S07] | Setting the default DPI awareness；manifest 声明与初始化顺序 |
| [S08] | LowLevelMouseProc；钩子线程、输入转交、返回值及超时 |
| [S09] | LowLevelKeyboardProc；键盘钩子与回调限制 |
| [S10] | CreateDIBSection；内存访问、同步与生命周期 |
| [S11] | BITMAPINFOHEADER；top-down 与像素布局 |
| [S12] | BitBlt；复制、CAPTUREBLT、裁剪与颜色管理边界 |
| [S13] | StretchDIBits 与 SetStretchBltMode；缓存绘制和像素缩放 |
| [S14] | Window Features；message-only 广播限制、layered 窗口及输入行为 |
| [S15] | SetWindowDisplayAffinity；捕获排除与系统版本限制 |
| [S16] | DwmFlush；桌面合成同步 |
| [S17] | GetDC 与 DeleteObject；线程/资源释放约束 |
| [S18] | OpenClipboard 与 SetClipboardData；所有权及失败语义 |
| [S19] | SetTimer 与 KillTimer；计时器及已排队消息 |
| [S20] | Notifications and the Notification Area；托盘生命周期 |
| [S21] | WTSRegisterSessionNotification；会话通知与配对注销 |
| [S22] | PROCESS_MEMORY_COUNTERS_EX；工作集与私有提交 |
| [S23] | GetGuiResources；GDI/USER 对象计数 |
| [S24] | Cargo Profiles 与 Rust Linkage；优化参数、panic 和 CRT |
| [S25] | std::sync::mpsc::SyncSender；有界通道的 try_send |
| [S26] | W3C CSS Color 4；RGB/HSL 格式与转换定义 |
| [S27] | UnhookWindowsHookEx；卸载时回调执行状态的约束 |
| [S28] | MSLLHOOKSTRUCT；鼠标事件的坐标、时间和滚轮字段 |
| [S29] | Rust for Windows CF_UNICODETEXT；剪贴板常量的 feature 归属 |
| [S30] | std::fs::rename；配置替换与错误处理 |

补充的同组文档：[Application manifests][S07b]、[SetStretchBltMode][S13b]、[DeleteObject][S17b]、[OpenClipboard][S18b]、[KillTimer][S19b]、[Rust Linkage][S24b]。

[S01]: https://docs.rs/crate/windows/0.62.2/source/Cargo.toml "Rust for Windows 0.62.2"
[S02]: https://docs.rs/embed-resource/3.0.11/embed_resource/ "embed-resource 3.0.11"
[S03]: https://learn.microsoft.com/en-us/windows/powertoys/color-picker "PowerToys Color Picker"
[S04]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getmessage "GetMessage"
[S05]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey "RegisterHotKey"
[S06]: https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows "High DPI Desktop Application Development"
[S07]: https://learn.microsoft.com/en-us/windows/win32/hidpi/setting-the-default-dpi-awareness-for-a-process "Setting the default DPI awareness"
[S07b]: https://learn.microsoft.com/en-us/windows/win32/sbscs/application-manifests "Application manifests"
[S08]: https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelmouseproc "LowLevelMouseProc"
[S09]: https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc "LowLevelKeyboardProc"
[S10]: https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-createdibsection "CreateDIBSection"
[S11]: https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-bitmapinfoheader "BITMAPINFOHEADER"
[S12]: https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-bitblt "BitBlt"
[S13]: https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-stretchdibits "StretchDIBits"
[S13b]: https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-setstretchbltmode "SetStretchBltMode"
[S14]: https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features "Window Features"
[S15]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity "SetWindowDisplayAffinity"
[S16]: https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmflush "DwmFlush"
[S17]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdc "GetDC"
[S17b]: https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-deleteobject "DeleteObject"
[S18]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setclipboarddata "SetClipboardData"
[S18b]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-openclipboard "OpenClipboard"
[S19]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-settimer "SetTimer"
[S19b]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-killtimer "KillTimer"
[S20]: https://learn.microsoft.com/en-us/windows/win32/shell/notification-area "Notifications and the Notification Area"
[S21]: https://learn.microsoft.com/en-us/windows/win32/api/wtsapi32/nf-wtsapi32-wtsregistersessionnotification "WTSRegisterSessionNotification"
[S22]: https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex "PROCESS_MEMORY_COUNTERS_EX"
[S23]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getguiresources "GetGuiResources"
[S24]: https://doc.rust-lang.org/cargo/reference/profiles.html "Cargo Profiles"
[S24b]: https://doc.rust-lang.org/reference/linkage.html "Rust Linkage"
[S25]: https://doc.rust-lang.org/std/sync/mpsc/struct.SyncSender.html "SyncSender"
[S26]: https://www.w3.org/TR/css-color-4/ "CSS Color Module Level 4"
[S27]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-unhookwindowshookex "UnhookWindowsHookEx"

[S28]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-msllhookstruct "MSLLHOOKSTRUCT"
[S29]: https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/System/Ole/constant.CF_UNICODETEXT.html "CF_UNICODETEXT"
[S30]: https://doc.rust-lang.org/std/fs/fn.rename.html "std::fs::rename"
