# 原生安全边界与所有权约定

本文随实现维护。生产技术栈仍为 Rust / windows 0.62.2 / Win32 / GDI；
`core`、`app/controller.rs` 和 `ui/pixel_effects.rs` 禁止 unsafe。
unsafe 数量不是安全性评分；原生资源、回调载荷和像素存储需要各自的证明。

## 窗口与回调

`ui/windows/window_lifetime.rs` 跟踪 `NotAttached → Alive(root) → Destroying(root) → Destroyed`。
结果、设置、实时预览和冻结放大镜的 Rust owner 唯一拥有稳定 Box。
USERDATA、content 和 subclass 只借用该分配，没有释放权。
预览和放大镜的 lifetime 在 `RefCell<State>` 外，终止处理不需要取得绘制状态的可变借用。

`WindowInit { state, role }` 只在同步 `CreateWindowExW` / `WM_NCCREATE` 中借用；
USERDATA 保存稳定 state 指针，绝不保存栈上 init。设置 USERDATA 前清 LastError，
用返回值与紧随其后的 LastError 一起判断成功。只有 Root 安装成功才能记录根 HWND。
构造返回错误也经 tracker 清理；root 创建后立即由 Rust owner 承接，后续子控件失败由同一 Drop 收尾。

根 `WM_NCDESTROY` 的默认过程链和 USERDATA 清理结束后才记录 Destroyed。
content 终止不结束 root。subclass 在自己的终止消息中移除并继续默认链。
所有共享 callback 的控件都必须是该 root 的后代；新增跨树引用必须重新审查这项证明。
回调捕获 panic 并 abort，不允许 unwind 穿过系统 ABI。

owner 先关闭新业务意图，在活 HWND 上停止 timer，结束 Ref/RefMut 借用，再同步销毁窗口树。
已经记录 Destroyed 时不再使用旧 HWND，包括 KillTimer 或清 USERDATA。
只有终止证据允许 callback、字体、图标和绘制资源析构。
`DestroyWindow` 失败且无根终止证据，或成功却没有终止证据，均输出最小诊断并 abort。
这是一项极端失败路径行为变化；普通捕获、创建、剪贴板忙和装饰失败仍按原错误/降级路径处理。
`IsWindow`、`WM_DESTROY`、尝试清 USERDATA 均不能替代本次绑定的根终止证据。

## GDI 所有权和恢复

`platform/windows/gdi.rs` 是生产 DC/bitmap/font/brush/pen 的唯一底层所有权实现：

| 资源 | 成对 API | 约束 |
|---|---|---|
| DesktopDc / WindowDc | GetDC / ReleaseDC | 同线程；WindowDc 的 HWND 在租用期存活 |
| MemoryDc | CreateCompatibleDC / DeleteDC | 私有 DC；不能用 DeleteDC 释放绘制或控件 DC |
| OwnedBitmap / Font / Brush / Pen | 创建 API / DeleteObject | 不接管 stock object；借用者结束后释放 |
| PaintSession | BeginPaint / EndPaint | 先建立会话再借用 callback 状态；无效 DC 也配对清理 |
| SavedDc | SaveDC / RestoreDC | 保存失败不构造有效 guard；显式 restore 返回结果，Drop 不重复恢复 |

owner 不可 Copy/Clone；线程绑定通过 `PhantomData<Rc<()>>` 保持。
从原始资源接管所有权的入口为 unsafe，必须是唯一的新建资源，不能把任意整数或 stock 句柄传入。
字体语言、字号和字重仍由 UI facade 决定。DPI/语言字体替换先创建全套、更新控件，最后释放旧 owner。

`BitmapDc` 同时拥有私有 MemoryDc、bitmap 和旧选中对象，所有权不可拆出。
正常销毁先恢复旧对象，再释放自有资源；临时 DIB 选择独占借用 surface 与临时 bitmap。
恢复失败会返回错误并使 surface 永久失效；先销毁私有 DC 来解除选择。
若 DC 销毁也失败，则保守保留可能关联的原生 bitmap，避免不确定选择关系下强制释放。
此路径可能泄漏原生资源，属于可观察的失效处理，不等同于 callback 状态的 abort 策略。
字体/笔的 DC 恢复失败也保留可能仍被选中的资源。借来的 paint/control DC 绝不 DeleteDC。
自绘按钮把字体恢复失败记录到所属 Theme 的不可清除标志；结果/设置窗口在替换旧字体集前，
以及原生窗口树结束后的字段析构前，检查该标志并保留可能有关联的字体。
失败标志不通过增加引用计数或注册表追踪句柄；此罕见分支允许原生字体泄漏以避免不确定释放。

短期绘制资源先声明，SavedDc 后声明，使恢复早于资源释放。原始 HDC 逃生入口仍是 unsafe：
不能保留句柄、擅自改选中关系或在像素切片存活时发起 GDI 访问。
本项目没有声称可随意遗忘 guard 的公共 safe API 自动证明全部原生安全。

## DIB 与纯像素算法

`platform/windows/dib.rs` 只支持 top-down 32-bit BI_RGB BGRX。
`Dib32Layout` 在任何 native 分配前检查正尺寸、i32、checked stride/长度、isize 和 u32 上限。
位图独占拥有像素分配；创建时检查非空指针并一次性初始化全字节。
原始切片构造集中在 PixelStorage 的两个短闭包入口，长度来自固定的已验证 layout。
切片生命周期受对应 owner 的独占借用约束；不可逃逸、不可与 GDI 写入重叠。
同步检查 GdiFlush；捕获或同步失败清除可读状态，禁止读旧像素冒充新采样。
BGRX 第四字节是存储字节，不能解释为来源图像的 alpha。

GdiSampler 创建并复用 1×1 表面；实时路径不创建 DC、bitmap 或 Vec。
冻结区域仍受 MAX_FREEZE_SIDE_PX、实际单显示器边界和间隙检查约束。
冻结临时 DIB 复用同一个 DC，先恢复 1×1 选择再释放；失败的表面不能继续采样。
FrostedPanel 用同一个 RefCell 保护 surface、scratch 和缓存 origin；捕获/模糊全部成功才提交 origin。
缓存借用内不执行会重入窗口的 USER32 操作。同位置复用，不增加 timer。

`ui/pixel_effects.rs` 不导入 Windows。它检查切片、尺寸、radius/累加器和透明度范围，
复用 scratch，保持原算法的整数舍入、BGR 顺序和 X 字节；UI 的透明度上限仍是 80%。
整个 ui 模块可参与非 Windows 构建，只有 ui::windows 有平台 gate。

## 消息载荷

`ui/windows/messages.rs` 的具名 unsafe 解码器限定 CREATESTRUCTW/WindowInit、
DRAWITEMSTRUCT、按钮 NMCUSTOMDRAW、DPI RECT。空值检查不是对任意地址有效性的验证。
调用必须来自对应消息的原生分发，满足布局、对齐和同步存活条件。
WM_NOTIFY 先读 NMHDR，再验证通知码、父控件关系、按钮类与样式，最后读完整 custom draw。
绘制 helper 接收类型化的短期结构；DPI 建议矩形复制成值后排队。
不把载荷地址、结构引用或无界可变 userdata 引用放进 pending 队列。

## 输入与内核事件

ControlSignal 唯一持有标准库 OwnedHandle；成功 CreateEventW 后仅一个所有权接管点。
跨线程 SetEvent 借用这个 owner，不增加手写 CloseHandle 或 unsafe Send/Sync。
JoinHandle 通过 AsHandle 提供 BorrowedHandle，controller 只转发借用。
host 和 resource-probe 只在联合等待调用旁建立短期 HANDLE 数组，返回后借用结束，再 join/修改 session。
这套句柄类型只用于 CloseHandle 对应的内核对象，不能用于 HWND/GDI/HICON。

输入钩子仅按低级钩子消息契约读取同步有效结构并复制值；不保存 native 临时指针。
原有限队列、移动合并、按下/释放配对、会话代际、原子顺序和有界退出协议保持不变。
没有新增后台线程、轮询定时器、每次等待 DuplicateHandle 或鼠标事件分配。

## 剪贴板及其余边界

Clipboard 为终止的 UTF-16 分配 movable GlobalAlloc，锁定后按已检查长度复制并正确处理最终 GlobalUnlock。
只有 SetClipboardData 成功才把所有权交给 Windows；之前的失败由唯一 owner 释放。
OpenClipboard/CloseClipboard 独立配对；有界重试、token/timer 过期防护不变。
配置/热键的事务顺序不变。图标直到窗口/托盘不再借用时才释放。
监视器枚举只在同步回调内短借用栈上上下文；其他 Win32 查询保持调用点的明确安全说明。

## 验证与维护

Cargo 对全目标启用 `unsafe_op_in_unsafe_fn = deny` 和
`clippy::undocumented_unsafe_blocks = deny`；不排除测试、示例或 build script。
故障注入、隐藏窗口开关和资源计数仅存在于测试构建，release 没有故障环境变量或普通命令行入口。
fatal 测试在专用子进程运行，ready/freed 标记与正常对照用于防止假通过。
桌面测试、合成图像测试、纯算法测试和实机性能分别记录，不相互冒充。

本轮的命令、退出码、限制和测量见
[实施报告](unsafe-refactoring-report-2026-09-27.zh-CN.md)。
原生依据：[DestroyWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-destroywindow)、
[WM_NCDESTROY](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-ncdestroy)、
[CreateDIBSection](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-createdibsection)、
[RestoreDC](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-restoredc)。
