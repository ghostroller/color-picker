# color-picker：unsafe 边界收敛与资源生命周期重构实施意见

> 用途：交给本地 agent 直接执行的重构任务书，不是已经完成的修复报告。
>
> 文档日期：2026-09-27。
>
> 核对基线：`1a302eecd4d9954205cabff39186955e1bd6dcd3`，`main`，v0.2.5；Rust edition 2024，`windows = "=0.62.2"`。
>
> 本文基于源码静态检查和官方接口文档。编写本文时没有运行 Windows 构建、测试、故障注入或性能测量。窗口销毁失败相关发现属于条件性生命周期风险，不是已经复现的正常路径 UAF。

## 1. 执行结论与范围

本轮保留 **Rust + windows crate + Win32 + GDI**，不更换 GUI 框架，不重写输入协议，不引入通用原生 UI 框架。

重构目标是：减少重复的资源管理和裸指针解释位置，使状态与资源的释放条件可证明，使纯算法脱离 FFI 审计范围。`unsafe` 关键字数量仅作辅助统计，不能作为唯一验收指标。

**本轮采用的唯一主路径：保留窗口状态由 Rust owner 持有的稳定 `Box`，补根窗口生命周期跟踪；集中 GDI 资源所有权；集中消息解码；抽离模糊算法；使用标准库表达内核句柄所有权；最后启用检查规则。** 不将窗口状态改为全局注册表，也不将所有窗口迁移成回调持有的 `Rc`。

### 1.1 不得改变的行为

- 保留 `core` 和 `app/controller.rs` 的 `#![forbid(unsafe_code)]`；不得通过降低 lint、换 edition 或宏隐藏不安全代码。
- 保留实时采样的 1×1 可复用表面、冻结矩形限制、实际显示器边界检查、冻结图像坐标映射，以及采样失败不冒充成功的行为。
- 保留输入手势按下/释放配对、会话代际、有限事件队列、移动事件合并和有界退出；不新增轮询定时器、后台线程或每次鼠标事件分配。
- 保留浮层非激活、捕获排除与隐藏/合成同步策略，保持采样图像与装饰背景分离。
- 保留剪贴板所有权交接、有界重试和过期 timer/token 防护；保留配置与热键更新事务。
- 保留当前中英文 UI、DPI 缩放、窗口外观、可访问性、快速复制路径与性能定位工具。

以上约束与当前项目的模块及实现相对应，入口见 [S02]—[S12]。凡是与本轮目标无关的功能或视觉调整，另行记录，不混进重构提交。

### 1.2 本轮明确不做

不升级 Rust edition、`windows` 或其他依赖；不增加运行时依赖；不引入 Tokio、跨平台窗口框架、智能指针注册中心或统一“万能句柄”。不把所有 Win32 调用机械地套上同名函数。不以进一步瘦身为由删除错误检查、`GdiFlush`、线程限制或现有回归测试。

纯代码提取只强制覆盖 `blur_and_tint` 及其直接需要的数据校验。结果窗/设置窗的大规模布局重排、控制器重写和全面模块拆分不属于本轮。

---

## 2. 开工检查：先确认实际代码，再执行任务

### 2.1 必做检查

先读取项目实际存在的 `AGENTS.md`、本机环境说明及仓库约定，执行：

```powershell
git status --short
git rev-parse HEAD
rustc -Vv
cargo -V
```

在实施报告记录 `START_SHA`、工作区已有修改、Windows 版本、工具链和测试环境是否有交互式桌面。

如果本地 HEAD 已超出本文基线，按本文列出的文件与符号重新核对。已经等效完成的任务记录为“已满足”，不要重复实现，更不能回退较新的修复。若基线对象已存在本地，可用 `git diff <本文基线>..HEAD -- <相关路径>` 辅助检查；对象不存在时不为此擅自执行远程写操作。

不覆盖用户未提交修改，不执行 `reset --hard`、强制 checkout 或未经授权的 stash。文档不是要求把仓库回退到 v0.2.5。

### 2.2 本机执行与远程操作限制

**Windows 本机直接运行原生测试和 MSVC 构建，不需要放进 Docker。只有用户明确要求时才执行 GitHub CI。** 不为验证主动推送、创建 PR、触发 workflow、打 tag 或发布 release；推送也可能间接触发 CI，因此不作为默认步骤。

遵循仓库已有本地提交规则；没有相反规则时，可以按后文独立提交边界创建本地 commit，但不要自动 push。只提交本任务修改。

遇到缺少 Windows SDK、桌面或另一版本 Windows，分别记录阻塞与 `NOT TESTED`。不能把静态检查、Linux 测试或编译成功当成 Windows UI 实测通过，也不能为了补齐环境而绕过上述限制启动 Docker/CI。

### 2.3 先留基线，再改动

执行一次当前环境可运行的基线验证，记录命令、退出码、已有失败和忽略项。记录生产代码、测试/示例代码各自的 unsafe 清单。

```powershell
rg -n '\bunsafe\b' src examples build.rs
rg -n 'from_raw_parts|from_raw_parts_mut|Box::from_raw|GWLP_USERDATA|SetWindowSubclass|SelectObject|DestroyWindow|CloseHandle' src examples
```

`rg` 结果只是词法清单，可能包含注释、字符串、`unsafe fn` 和 `unsafe impl`，不是精确的块数或安全性评分。没有 `rg` 时用本机现有等价工具，不因统计工具缺失中断任务。

---

## 3. 当前代码定位与修改目标

| 编号 | 基线文件/符号 | 已核对的现状 | 修改目标 |
|---|---|---|---|
| R01 | `ui/windows/result.rs::ResultWindow::drop`，`settings.rs::SettingsWindow::drop` | 忽略 `DestroyWindow` 结果，随后状态/字体等字段会正常释放 | 只有确认原生引用结束后才允许字段析构；失败时有统一、不可误解的策略 |
| R01 | `preview.rs::PreviewWindow`，`magnifier.rs` 对应窗口 owner | 浮层已有稳定状态分配；预览窗销毁失败后尝试清空 userdata | 不把“尝试清空”当成释放证明；统一根窗口结束跟踪 |
| R02 | `capture.rs`、`drawing.rs`、`magnifier.rs`、`frost.rs` | DC、位图、恢复选中对象和失败清理逻辑重复 | 专用资源 guard 只维护一份，调用者不再重复所有权释放代码 |
| R02 | `drawing.rs::OwnedFont`、`theme.rs::Font` | 各自销毁字体；字体策略并不完全相同 | 统一底层所有权，保留各自字体选择策略 |
| R03 | `GdiSampler`、`FrostedPanel` | DIB 指针、尺寸、同步与资源选择关系由调用点维护 | 把像素存储有效性与选中生命周期集中，保留现有采样算法和资源复用 |
| R04 | `draw_swatch`、`draw_caption_button`、`theme::custom_draw*`、`draw_button` | 普通绘制函数接收 `LPARAM` 并解引用 | 回调边界解码一次，绘制层接收类型化数据 |
| R05 | `frost.rs::blur_and_tint`、`FrostedPanel::paint` | 纯算法与 Windows 实现同文件，调用被大 unsafe 块包围 | 提取可跨平台测试的纯算法，缩小 unsafe 覆盖范围 |
| R06 | `input.rs::ControlSignal`、`InputSession::wait_handle` | 内核事件存为 `usize`，工作线程等待句柄裸返回 | 使用 `OwnedHandle`、`BorrowedHandle<'_>`，保持协议不变 |
| R07 | `Cargo.toml`、`scripts/verify-windows.ps1` | 已有严格 Clippy 和 Windows 验证，但未显式启用 unsafe 注释检查 | 最后统一启用 lint，补安全证明及回归证据 |

对应源码见 [S02]—[S12]。`magnifier.rs` 的具体内部类型名由执行 agent 在当前 HEAD 搜索确认；不要为了匹配本文命名重命名整个模块。

---

## 4. 目标文件结构与依赖方向

以下为本轮建议新增/修改的落点。新文件名是实施建议，不是声称当前仓库已有这些文件。

```text
src/
  lib.rs                              # 解除整个 ui 模块的 Windows 限制；见 R05
  platform/windows/
    mod.rs                            # 注册 crate 内部 gdi、dib 模块
    gdi.rs                            # 新增：专用资源所有权、绘制/选择作用域
    dib.rs                            # 新增：32-bit DIB 布局、存储和选中表面
    capture.rs                        # 迁移资源复用和像素访问
    input.rs                          # 内核事件和线程等待句柄
    host.rs                           # 等待处的窄生命周期适配
  ui/
    mod.rs                            # Windows UI 保留 cfg；纯算法模块无 cfg
    pixel_effects.rs                   # 新增：安全的 blur_and_tint 和测试
    windows/
      mod.rs                          # 注册 window_lifetime、messages
      window_lifetime.rs              # 新增：根窗口状态跟踪与销毁策略
      messages.rs                     # 新增：少量、具名的原生消息解码函数
      drawing.rs
      frost.rs
      preview.rs
      magnifier.rs
      result.rs
      settings.rs
      settings_preview.rs
      theme.rs
  app/controller.rs                   # 只适配借用等待句柄，不引入 unsafe
```

依赖必须朝这个方向：

```text
纯数据/算法 ← UI 业务逻辑 ← 原生回调入口
                    ↓
        platform/windows/gdi + dib
```

`gdi.rs`、`dib.rs` 不依赖 `ui::windows`，不导入界面主题或应用语言。资源创建参数由上层传入。新底层 API 默认 `pub(crate)` 或更小可见性；不要扩大公共库 API。

这些模块只是本项目需要的小型基础设施，不设计泛型资源注册器、任意消息反射器或通用 Window<T> 框架。

---

## 5. R01：修复窗口状态释放条件

### 5.1 风险定位与本轮策略

结果窗和设置窗的同一 `CallbackState` 被主窗口、内容窗口以及 subclass 引用；其中 `Theme`、字体等也被原生控件借用。[S02][S03][S04]

`DestroyWindow` 有失败返回；根窗口的 `WM_NCDESTROY` 在其子窗口销毁后到达。[A01][A02] 因此，不能把“调用过销毁函数”“主窗口 userdata 清零”或“已经收到 WM_DESTROY”当成可以释放整棵原生引用树的充分条件。

**本轮选择：正常销毁按原路径完成；若销毁失败且无法证明对应根窗口生命周期已经结束，则在释放 callback、字体、图标、绘制资源之前记录最小诊断并 `std::process::abort()`。**

这是仅针对“即将释放仍可能被原生回调引用的 Rust 状态”的不可恢复分支，不是所有 Win32 失败的通用处理方式。普通创建失败、捕获失败、剪贴板忙、装饰绘制失败仍按原错误/降级逻辑处理。

选择这个策略，是为了保持现有 owner 模型，不新增“失败窗口常驻隔离区”及其连带资源回收协议。当前回调的 panic 边界已经使用 abort；但这不代表本轮新增销毁策略无需说明：实施报告必须明确记录这一极端路径的行为变化。[S02][S04]

### 5.2 状态模型

在 `window_lifetime.rs` 增加小型根窗口跟踪类型，状态至少区分：

```text
NotAttached
  └─ 根窗口成功安装 userdata → Alive(root_hwnd)
       ├─ 开始销毁 → Destroying(root_hwnd)
       └─ 根窗口 WM_NCDESTROY 完成 → Destroyed
```

跟踪类型持有 `Cell` 等内层可变状态，放在稳定的 callback 分配中。它本身不是 HWND owner，也不自己析构窗口；窗口 owner 仍是唯一负责最终释放 `Box` 的对象。

必须区分 root 和 content：结果窗与设置窗复用同一个窗口过程和 callback 指针，内容窗口的 `WM_NCDESTROY` **不得**把整棵窗口树标为已销毁。

建议用同步创建参数 `WindowInit { state_ptr, role: Root | Content }` 显式区分创建角色。该临时参数只在 `CreateWindowExW` 同步创建期间被 `WM_NCCREATE` 读取；只把稳定的 state 指针存入 userdata，**不保存栈上 WindowInit 的地址**。随后用已记录的 root HWND 判断终止消息身份。沿用现有等价机制也可以，但必须证明不会把尚未赋值的 viewport 当成 root。

对预览/放大镜，不要把 lifecycle 放到销毁回调必须 `borrow_mut()` 才能取得的 `RefCell<State>` 内。采用外层稳定数据：

```text
Box<CallbackData> {
    lifetime: WindowLifetime,
    state: RefCell<State>,
}
```

这样销毁处理不需要与绘制状态争用可变借用。保持窗口和资源 `!Send / !Sync` 的线程约束。

### 5.3 构造路径实现顺序

1. 创建稳定 callback 分配与 `NotAttached` 跟踪器。
2. 以 `*const` 指针传给原生创建函数；回调只借用状态，不获得释放权。
3. `WM_NCCREATE` 安装 userdata 成功后记录根窗口身份。`SetWindowLongPtrW` 的零返回有歧义，必须按“先清 LastError，再检查返回值与错误码”的规则处理。[A03]
4. `CreateWindowExW` 成功后立即构造 Rust window owner，再进行 DWM、字体和子控件等可能失败的操作。
5. **不要直接用一个未经检查的 `CreateWindowExW(...)?` 跳过已安装状态的清理判断。** 在创建返回错误时，核对 tracker：未安装或已结束才能释放；仍标为 Alive 则在 callback 有效期间执行统一销毁流程。
6. 后续任意 `?` 都通过同一个 owner 的析构逻辑处理部分创建的子窗口，不再复制一套临时资源清理。

构造失败处理可以使用一个很小的创建 guard，或显式 match；不需要构造自引用结构。无论采用哪种写法，必须覆盖“root 已创建、content 或第 N 个控件失败”的路径。

### 5.4 销毁路径实现顺序

所有四类窗口统一为：

```text
读取生命周期；若已经 Destroyed，跳过对旧 HWND 的任何操作
→ 关闭新的业务排队/回调意图
→ 在 HWND 仍有效时停止本窗口 timer 等附属活动
→ 结束所有 Ref/RefMut 借用
→ 标记 Destroying，调用 DestroyWindow
→ 同步处理 child/subclass/root 的销毁消息
→ 根窗口终止回调返回，tracker 确认 Destroyed
→ owner 才允许释放 callback、字体、图标和绘制资源
```

根窗口 `WM_NCDESTROY`：保留现有 default procedure 链；清空本窗口 userdata；不要 `Box::from_raw`；只在本次终止处理结束、即将返回时记录 root 终止。不能在原生回调中释放 callback。

subclass：继续按现有约定移除 subclass 并调用 `DefSubclassProc`。根窗口生命周期证明依赖子控件确实属于该 root；新增跨树引用时必须重新审查，不能机械沿用 tracker。

`DestroyWindow` 返回失败后：重新读取 tracker；只有此前已经获得该根窗口的终止证据，才可安全跳过。否则进入前述 abort 分支。不要仅依赖 `IsWindow`：它不是对该次 Rust owner/原生窗口绑定的生命周期证明，也不能解决句柄复用问题。

销毁成功后却未观察到本项目跟踪的终止事件，视为跟踪契约被破坏，不悄悄继续释放。诊断记录窗口种类、状态、API 结果，不 dump callback 内存。致命日志路径不得触发 UI、重入或因日志失败绕过 abort。

### 5.5 不允许的“简化”

不在 Drop 中 `panic!` 作为可捕获错误；不以“已经尝试 DestroyWindow”为理由返回。不要只泄漏 `Box<CallbackState>` 却继续释放它依赖的字体/图标。不要在销毁后对已复用 HWND 清 userdata。不要把 `&mut State` 跨 `DestroyWindow`、`SendMessageW` 或其他可能同步重入的调用持有。

### 5.6 验收

验证正常 owner drop、部分创建失败、root 先被其 owner 销毁再 drop Rust 对象、content 独立销毁不误判 root，以及销毁失败时不会进入字段释放。具体测试见第 13 节。

---

## 6. R02：统一 GDI 资源所有权与作用域

### 6.1 先移动基础实现，后迁移调用者

新增 `platform/windows/gdi.rs`，先从已有实现提取以下确实重复或直接需要的专用类型：

| 类型 | 获取/创建来源 | 释放责任 | 关键限制 |
|---|---|---|---|
| `DesktopDc` | `GetDC(None)` | `ReleaseDC(None, dc)` | 同线程；不是 `DeleteDC` |
| `WindowDc` | `GetDC(Some(hwnd))` | 与原 HWND 配对的 `ReleaseDC` | 借用窗口在会话内存活 |
| `MemoryDc` | `CreateCompatibleDC` | `DeleteDC` | 自己拥有，不承担位图/字体所有权 |
| `OwnedBitmap` | 自建 bitmap/DIB 的 bitmap 句柄 | `DeleteObject` | 不接管 stock bitmap；释放前解除选中关系 |
| `OwnedFont` | `CreateFontW` 等返回的字体 | `DeleteObject` | 不接管 stock font；控件仍引用时不得释放 |
| `OwnedBrush` / `OwnedPen` | 实际创建的 brush/pen | `DeleteObject` | 仅迁移已有创建点，不把 stock object 包成 owner |
| `PaintSession` | `BeginPaint` | `EndPaint` | 只用于有效窗口的绘制会话，不用 ReleaseDC |
| `SavedDc` | `SaveDC` | 对应层级的 `RestoreDC` | 保存失败不构造“有效 guard” |

不为没有使用场景的资源类型增加封装。资源释放规则应按对应 API 契约保留；尤其 `GetDC` 与 `CreateCompatibleDC` 的结果不能使用相同销毁函数。[S05][S06][A05][A06]

错误处理也要保留每个 API 的真实返回约定：不要一律调用 `Error::from_thread()`。对不保证设置 LastError 的失败，使用明确的项目错误说明，避免报告陈旧错误码；`SelectObject` 的无效返回判定应与本模块实际选用的对象类型一致。

所有 owner 禁止 `Copy` 和自动按位 `Clone`。原始句柄接管入口保持私有或 `unsafe`，写明来源、独占所有权和正确销毁函数。调用者不得用一个任意整数通过公共 safe 构造函数制造“有效资源”。线程绑定资源保留显式线程限制。

### 6.2 选中关系必须一起建模

`OwnedBitmap` 单独存在不保证安全：位图可能仍被某个 DC 选中。长期选中的后备缓冲由一个组合对象拥有 DC、bitmap 和原选中对象；临时替换采用作用域选择 guard。

组合对象不持有指向自己其他 Rust 字段的引用；保存的是原生句柄值，构造完成后不再公开拆出所有权。需要 DIB 的组合见 R03。

正常释放顺序：恢复原选中对象，再释放自有位图与 DC。构造时先准备可失败的资源，再建立受 guard 保护的选中关系；任何中途返回都不能漏掉恢复。

恢复失败不能记录为成功。对**自己拥有的私有 MemoryDc**，可以通过销毁该 DC 解除选择后再释放位图；若连这一点也不能确认，保守保留仍有关联的原生资源并记录失败，不强行释放。不要对借来的 paint/control DC 调用 `DeleteDC`。调用者也不得继续复用被标记为失效的表面。

这是原生资源泄漏/失效处理，不套用 R01 的 callback 状态释放策略。各 API 失败分支必须区分，不要求所有 GDI 错误 abort。

### 6.3 迁移绘制会话

把 `drawing.rs::PaintSession` 移到公共 GDI 模块，迁移预览窗、放大镜、`paint_result`、`paint_content`、`Theme::paint` 的手写配对。[S06][S07][S10]

**保留“先 BeginPaint，再借用状态”的顺序。** `PaintSession` 必须先建立，再取得 `RefCell` 绘制借用；绘制借用先释放，最后 `EndPaint`。不要把 guard 塞进已持有 `RefMut` 的 helper 中。[S04]

`BeginPaint` 返回无效 DC 时不继续调用绘图；对已经开始的会话保留匹配的清理行为。不要在统一返回 `Result` 时遗漏失败路径的配对。

将需要还原颜色、字体、笔、viewport 等 DC 状态的代码迁移到 `SavedDc`。保存失败时保留原有“返回错误/交给默认绘制”的语义，不能在未保存状态时继续修改借来的 DC。

**局部变量析构顺序必须检查。** 新创建的 pen/font 要活到恢复以后；例如先创建 pen，后创建 SavedDc，再选入 pen，正常离开作用域时先恢复、后释放 pen。不能把资源声明在 guard 后面，然后误以为所有 RAII 顺序都自动正确。

guard 的 restore 失败需要可观察。关键绘制路径用显式 `finish/restore` 返回错误；Drop 只做未完成时的兜底，不重复恢复。不能声称一段调用者可随意遗忘 guard 的公共 safe API 已经自动获得完整内存安全证明。

### 6.4 字体策略保留在 UI

`OwnedFont` 只管理字体所有权。`drawing.rs` 保留 overlay 字号/字重策略，`theme.rs` 保留 UI 语言、mono/Consolas 等选择策略，最后都创建/持有公共 owner。[S06][S10]

DPI/语言更新应先创建整套新字体，再让相关控件使用新字体，最后释放旧字体。不能在发完一部分 `WM_SETFONT` 后因错误先释放全部旧字体。不要因为统一字体 owner 改变实际字体、字号或字重。

### 6.5 迁移顺序

`gdi.rs` 基础实现与测试 → `drawing.rs`/`theme.rs` → 预览/放大镜调用点 → 结果/设置绘制会话 → `capture.rs`/`frost.rs` 的公共资源导入。

同一种所有权 guard 迁移后删除旧实现，禁止新旧并存作为最终状态。可保留上层策略 facade，但不能各自再实现资源 Drop。

---

## 7. R03：统一 DIB 布局与 CPU/GDI 访问边界

### 7.1 目标与落点

新增 `platform/windows/dib.rs`，负责本项目使用的 **top-down、32-bit、BI_RGB、BGRX** 存储，不扩展到调色板、任意 bit depth 或图像格式库。

实现三个明确责任：

- `Dib32Layout`：校验尺寸、stride 和总字节数，不持有指针。
- `OwnedDib32`：唯一拥有自建 DIB bitmap，保存由该 bitmap 支撑的 `NonNull<u8>` 与已经校验的布局。
- `Dib32Surface`：组合 MemoryDc、OwnedDib32 和原选中对象，约束 GDI 操作与 CPU 访问的互斥。

与兼容位图后备缓冲共用 R02 的句柄/选择基础设施，不为共用几十行而建立多层泛型层次。原生 DIB 像素地址由 bitmap 的原生分配决定，不是对 Rust struct 自身的引用。

### 7.2 布局验证

在创建 native bitmap 之前完成：

```text
width、height > 0
width、height 可无损表示为 GDI 使用的正 i32
stride_bytes = checked_mul(width, 4)
len = checked_mul(stride_bytes, height)
len <= isize::MAX
如填写 biSizeImage，则 len 还须可表示为 u32
biHeight 使用经过验证的 height 取负，不对 i32::MIN 取负
```

保留捕获模块自己的 `MAX_FREEZE_SIDE_PX` 与单显示器约束；通用存储有效不代表允许扩大冻结截图。

32 位每像素恰好四字节，本实现使用 `width * 4` 的行宽。`CreateDIBSection` 成功后也检查输出指针非空。不能由调用者每次重新传一个可能不一致的长度。

在首次把整个存储变成 `&[u8]`/`&mut [u8]` 之前，必须证明其字节已初始化。必要时在创建阶段一次性初始化，而不是每次采样清零。BGRX 的 X 通道不是来源图像的 alpha，不能因封装把它解释为真实透明度。[S05][A04][A10]

### 7.3 CPU 访问契约

像素访问接口必须借用对应 owner，写访问要求独占借用；禁止 `&self -> &mut [u8]`，也禁止返回 `'static` 切片或允许切片活过 bitmap 销毁。

GDI 刚写过 DIB 时，先检查 `GdiFlush` 成功，再开放 CPU 读取/写入。同步失败时返回错误，不读旧数据冒充新数据。不要用 `DwmFlush` 代替 `GdiFlush`：前者涉及合成可见性，不是此处 DIB CPU 存储访问的替代契约。[S05][S08][A04]

优先用短生命周期访问闭包或明确的借用 view：CPU 切片存在期间，不能通过 safe API 同时对同一 DIB 发起 GDI 写入、重新选择 bitmap 或销毁表面。获取 raw DC 的逃生入口只留在小范围内部，调用者仍承担相应 unsafe 契约。不要把裸 HDC 导出后宣称类型系统已阻止所有别名访问。

### 7.4 迁移 GdiSampler

保持 `new` 一次创建并复用 1×1 存储，`sample_pixel` 不创建 DC/bitmap/Vec。实时路径改成：

```text
校验显示器 → BitBlt 到复用表面 → GdiFlush 检查 → 读取 B/G/R 三个通道
```

`capture_rect` 继续使用临时 DIB，并在同一个 MemoryDc 上进行作用域内替换，保留现有资源复用，**不要默认改为每次冻结新建额外 DC**：

```text
校验冻结区域
→ 创建临时 OwnedDib32
→ 独占借用采样表面/DC，建立临时选择 guard
→ BitBlt / GdiFlush
→ 在临时 DIB 有效时复制为 FrozenImage
→ 先恢复 1×1 位图，再释放临时 DIB
```

临时选择 guard 同时约束临时 bitmap 和原 surface 的使用期；guard 仍存活时不能再从 sampler safe 调用 `sample_pixel`。内部实现可以是具名 `TemporaryDibSelection<'_>`，但不要为了绕过借用检查新增裸指针别名。

覆盖 BitBlt、GdiFlush、目标 Vec 分配/复制前失败，以及正常返回的恢复顺序。恢复失败后，sampler 不得继续被当作有效 1×1 表面；使本次捕获失败并按现有错误路径重建或结束会话。

### 7.5 迁移 FrostedPanel

将 `bitmap/dc/screen/previous/pixels` 等散落字段改为明确拥有的 DC 和 DIB 表面。`scratch`、缓存 origin、radius、transparency 仍由 FrostedPanel 管理。

要把独占 CPU/GDI 访问纳入真实借用：可将 surface 与 scratch/origin 放进同一个 `RefCell<BackdropState>`，保留 `paint(&self)`，绘制时只取得一次短借用；不要用另一个不相关的 `RefCell<Cache>` 假装保护可变像素。该借用内不调用会重入本窗口的 USER32 操作。

`paint` 的顺序：

```text
安全计算：坐标、是否需要刷新
→ 捕获失败前将 cache.origin 置为 None
→ 小 unsafe 范围：GDI 捕获与同步
→ 受独占借用约束的像素访问
→ safe：blur_and_tint
→ 成功后提交 cache.origin
→ 小 unsafe 范围：绘制到目标 DC
```

同 origin 不重复捕获/模糊，不添加 timer。任一刷新失败后不能保留“缓存有效”的错误标志，继续保留上层的不透明背景降级。

---

## 8. R04：把消息解码限制在回调边界

### 8.1 原则

把 `LPARAM` 视为带有原生调用契约的消息载荷，而不是任何普通函数都可以解释的整数。

`as_ref()` 或检查非零只能处理空指针，不能验证任意非空地址。类型化封装不得把“任意整数地址”变成声称安全的输入接口。[A10]

### 8.2 实现路径

在 `ui/windows/messages.rs` 只加入本项目实际需要的具名解码器，例如窗口创建、owner draw、按钮 custom draw 和 DPI 建议矩形。不提供 `fn decode<T>(lparam) -> &'static T` 一类万能转换。

解码器如果仍接收裸 `LPARAM`，应是窄作用域 `unsafe fn`，用 `# Safety` 写明有效消息类型、结构大小/对齐、生命周期与来源。回调的消息匹配分支负责满足该前提。

建议 `WM_NOTIFY` 的处理顺序：读取合法的 NMHDR → 检查通知码 → 确认对应的本项目按钮/控件类型 → 再按该通知实际布局读取 NMCUSTOMDRAW。不要未检查类型就把所有通知都解释成完整的 NMCUSTOMDRAW。

`WM_DRAWITEM` 验证当前消息与目标控件，再把有效 `DRAWITEMSTRUCT` 的所需字段传给绘制函数。保留现有控件 ID、控件类型与默认处理逻辑。

改变接口方向：

```rust
// 改造前：普通绘制函数自行解释外部地址。
fn draw_swatch(lparam: LPARAM, color: COLORREF) -> LRESULT;

// 改造后方向：只接收已在本次回调中解码的结构。
fn draw_swatch(draw: &DRAWITEMSTRUCT, color: COLORREF) -> LRESULT;
```

上例是目标接口示意，不是可直接编译的补丁。也可把用到的 rect、DC、ID、状态位复制成小型值对象，但不将消息对象的引用或它携带的指针保存到 pending 队列。

`draw_caption_button`、`theme::custom_draw`、`custom_draw_minimal`、`draw_button` 同步迁移，使一次消息不在多层函数反复转指针。图形绘制中的 Win32 调用仍可以包含 unsafe；目标不是让整个绘制模块零 unsafe。

`WM_DPICHANGED` 继续在回调里复制 `RECT` 后排队；禁止把 lparam 地址排队。`CREATESTRUCTW` 中的创建参数也只同步读取。

### 8.3 userdata 与回调入口

每一类窗口仅保留清晰可定位的 userdata 设置/取回路径，和 R01 的稳定状态模型一致。禁止从 HWND 构造生命周期不受约束的可变引用，禁止 `&'static mut State`。

对于当前普通 `fn window_message(...)` 内部直接解码原生指针的模式，将原生分发边界标为具契约的 `unsafe fn`，或者在 extern 回调内先解码后调用 safe 逻辑。不要只是把 unsafe 藏进普通函数，然后将调用者的责任从审计中抹掉。

保留现有 `catch_unwind`/abort 回调边界，不让 Rust unwind 穿过不支持 unwind 的系统 ABI。不得因新增 helper 把调用 `DefWindowProcW` / `DefSubclassProc` 的顺序改乱。

输入钩子本轮只核对“按钩子契约读取短期结构，复制值，不保存临时指针”，不顺手重写钩子状态机或原子同步。

### 8.4 验收

生产绘制 helper 不再接收需要自行解引用的 `LPARAM`。支持的消息、通知类型和回调生命周期都有明确注释。测试只使用正确构造、实际存活的消息结构及合规原生消息，不靠解引用随意整数地址来测试“防崩溃”。

---

## 9. R05：抽离纯像素算法，并真正参与非 Windows 编译

### 9.1 文件改动

新增 `src/ui/pixel_effects.rs`，迁入 `blur_and_tint` 与现有测试，文件头添加：

```rust
#![forbid(unsafe_code)]
```

保留其纯像素职责，参数为切片、尺寸、radius、transparency，不使用 HDC、HWND、COLORREF、Windows Error 或 Windows crate。

**必须一起调整模块注册。** 当前 `src/lib.rs` 对整个 `ui` 有 `#[cfg(windows)]`，仅移动函数仍不会让它参与非 Windows 测试。[S13][S14]

目标：

```rust
// src/lib.rs：ui 自身可跨平台；platform 的 cfg 保持原状。
pub mod ui;

// src/ui/mod.rs：只将原生 UI 限制为 Windows。
pub(crate) mod pixel_effects;

#[cfg(windows)]
pub mod windows;
```

不要重复注册已有模块；调整后核对整个 ui 模块没有通过其他导入把 Windows 依赖带进非 Windows 构建。

### 9.2 算法校验与一致性

保留现有滑动盒式模糊、整数舍入、BGR 通道顺序及第四字节处理。迁移前后对相同输入逐字节比较，不能用“肉眼看着相似”替代。

检查 width/height 非零、切片长度匹配、尺寸乘法不溢出、radius/count/累加器运算可表示、透明度合法。非法输入给出清晰错误或由已验证输入类型阻止，不用 saturating/clamp 悄悄掩盖调用错误。

保留 1×1、1×N、N×1 和 radius 大于图片边长的现有合法行为。调用方现有透明度上限不变；不因为抽函数放开 UI 参数范围。

本步骤不新增每次 paint 的 scratch 分配，继续复用 scratch。

---

## 10. R06：用标准库表达内核句柄所有权

### 10.1 ControlSignal

`ControlSignal(usize)` 改为持有 `std::os::windows::io::OwnedHandle`。在 `CreateEventW` 成功后，以一个明确的 unsafe 所有权接管点构造 OwnedHandle，确保只有一个 owner；随后删除手写 `CloseHandle` 的 Drop。[S09][A07]

保留 `ControlSignal::new`、`set` 等业务接口。调用 `SetEvent` 或等待 API 时，通过短期借用得到原生 HANDLE。

`OwnedHandle` 支持跨线程持有内核句柄，这不等于可以给 HWND/GDI 类型添加 `unsafe impl Send/Sync`。`Shared` 的已有原子顺序、队列和输入退出行为不改。

**OwnedHandle 只用于适合 CloseHandle 的内核对象；禁止用于 HWND、HDC、HBITMAP、HFONT、HBRUSH、HPEN、HICON。**[A07]

### 10.2 工作线程等待句柄

把接口调整为：

```rust
pub fn wait_handle(&self) -> Option<BorrowedHandle<'_>>;
```

优先由 `JoinHandle::as_handle()` 借出，不从其 raw handle 再制造 OwnedHandle；后者会造成错误的第二个关闭责任。[A08]

沿调用链检查并适配：

```text
InputSession::wait_handle
→ app/controller.rs 的等待句柄转发
→ platform/windows/host.rs 的消息队列 + 工作线程联合等待
```

借用结束后才能进行 `try_join` 或其他需要可变访问 session 的操作。不要用 `'static`、`transmute` 或“先存成 usize”绕过新出现的借用错误。

仅在最靠近 Win32 等待函数的局部作用域构造原生 HANDLE 数组；等待返回后数组不逃逸。controller 仍然不含 unsafe。

禁止为了避免生命周期约束每次等待都复制一个内核句柄；禁止把等待改成轮询或同步阻塞 join。

---

## 11. R07：安全注释、lint 与剩余边界登记

### 11.1 最后收紧全项目检查

主体迁移完成后在 Cargo.toml 合并（而不是重复添加）如下配置：

```toml
[lints.rust]
unsafe_op_in_unsafe_fn = "deny"

[lints.clippy]
undocumented_unsafe_blocks = "deny"
```

`undocumented_unsafe_blocks` 不是单独 `-D warnings` 就会自动启用的检查。[A09] 最终应在 `--all-targets` 下生效；不能排除测试、示例或 build script 来获得绿色结果。

为本轮未改语义但被 lint 暴露的边界补必要注释可以做；不要借机重构全部模块。禁止用 crate/module 级 allow 一次性绕过检查。若确有 lint 误判，只接受最窄位置、附原因且不掩盖实际证明缺失的例外。

### 11.2 注释必须回答实际前提

按操作需要写明：谁拥有对象，当前地址为何有效，长度与初始化如何成立，哪一方可能保留引用，何时释放，线程要求以及同步/重入条件。

不接受下列模板：

```rust
// SAFETY: Windows API is unsafe.
// SAFETY: pointer is not null.
```

对每个私有 unsafe helper 也写 `# Safety`；不依赖 lint 是否默认检查私有函数。公共文档写“调用方需确保……”但函数仍是完全不验证的公共 safe 原始指针入口，也不能视为正确封装。

一个块内只有短小、共享同一前提的原生操作可以合并；安全布局、颜色算法、队列更新和缓存逻辑不应为了少一个关键字被包进大 unsafe 块。

### 11.3 需要保留的 unsafe

原生 API 调用、消息和 callback 参数解释、DIB 存储映射、资源接管/释放仍可能需要 unsafe。不要把“全部移进 gdi.rs”当成强制指标；UI 边界调用仍可在本地保持小且有据可查的 unsafe。

把这些边界与对应的安全前提记录到新增 `docs/unsafe-boundaries.md`。至少记录窗口、GDI/像素、剪贴板、输入钩子和内核事件五类。明确哪些 safe 层依赖这些边界，而不是逐行复制源码。

---

## 12. 执行顺序、提交边界与并行限制

| 阶段 | 工作 | 建议本地提交主题 | 进入下一阶段条件 |
|---|---|---|---|
| P0 | 环境、HEAD、现有失败、资源/unsafe 清单 | 通常只更新实施记录 | 基线与不可测试范围明确 |
| P1 | R01 生命周期与异常退出测试 | `fix: enforce native window teardown lifetime` | 不释放仍可能被回调引用的状态；构造失败路径覆盖 |
| P2 | R02 公共 GDI owner/guard，迁移绘制会话与字体所有权 | `refactor: centralize GDI resource ownership` | 旧重复 guard 已删，恢复顺序可验证 |
| P3 | R03 DIB 布局、采样及 FrostedPanel | `refactor: bound DIB access and selection lifetimes` | 同步/长度/恢复测试通过，无实时路径额外分配 |
| P4 | R04 消息解码与类型化绘制 | `refactor: localize native message decoding` | 裸载荷只在具契约的入口解释 |
| P5 | R05 纯算法与 cfg 调整 | `refactor: isolate safe pixel effects` | 字节一致性和边界输入测试通过 |
| P6 | R06 标准库内核句柄 | `refactor: encode input handle ownership` | 等待/退出逻辑保持原行为 |
| P7 | R07 lint、全量回归、实施报告 | `chore: enforce and document unsafe boundaries` | 完成第 14 节验收，不将 NOT TESTED 写成 PASS |

每个提交应是可审查的逻辑单元；合理的小型依赖调整可合并，不能为了凑表格制造不可构建的中间提交。不要在每完成一阶段后停下来索要确认；在已授权范围内连续推进到实现、验证与报告。

使用多个 agent 时，先由主 agent 完成生命周期方案和公共 GDI/DIB 接口。之后纯算法/测试、句柄适配可并行；`result.rs`、`settings.rs`、`drawing.rs`、`theme.rs` 等共享文件不能让多个 agent 同时独立大改。主 agent 负责整合、一次完整验证和最终生命周期复核。

无论是否并行，都不把本轮拆成“只写计划、等待用户再下任务”。如果确实遇到环境或代码阻塞，保留已验证改动，准确记录阻塞，不伪造完成。

---

## 13. 测试方案与具体命令

### 13.1 现有 Windows 基础验证

仓库已有脚本会运行 fmt、Clippy、测试、release 构建，并验证实际嵌入的 manifest 与进程 DPI 上下文。[S11]

在仓库根目录、Windows 原生环境执行：

```powershell
# 使用本机已有 PowerShell；以下任选本机实际安装的宿主运行，不修改执行策略。
& .\scripts\verify-windows.ps1
```

定位失败时，脚本内的主要命令为：

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked --target x86_64-pc-windows-msvc
```

不要把分项构建通过替代脚本后续的 manifest/DPI 检查。不要硬编码 `target` 目录，沿用脚本通过 Cargo metadata 解析实际输出路径的方式。

### 13.2 新增测试矩阵

以下是本轮必须实现或提供等效覆盖的测试目标；名称可按当前测试模块风格调整。

| 类别 | 场景 | 必须验证什么 |
|---|---|---|
| 生命周期 | root 正常销毁 | child/subclass 终止先于 callback/字体释放 |
| 生命周期 | root 已由 native owner 销毁后再 drop Rust owner | 不再次操作旧 HWND；状态只释放一次 |
| 生命周期 | content 独立终止 | 不将 root 标为 Destroyed，不提前释放共享状态 |
| 生命周期 | root、viewport、第 N 个控件创建失败 | 部分创建树被安全结束，状态不提前释放 |
| 生命周期 | DestroyWindow 被测试 seam 注入失败且 root 仍活着 | 进入 fatal 分支，绝不执行受保护字段的释放 |
| 重入 | BeginPaint 的同步消息与 DPI/布局消息 | 不持有冲突 RefMut，不在状态借用内创建绘制会话 |
| GDI | 每个资源构造阶段失败 | 已创建的自有资源各释放一次，borrowed/stock 不释放 |
| GDI | SaveDC/SelectObject 失败与提前返回 | 不伪装保存成功；恢复按正确顺序发生 |
| DIB | 零/负/超大尺寸与乘法溢出 | 在 native 分配或切片构造之前失败 |
| DIB | BitBlt/GdiFlush/复制分配失败 | 不读无效或旧像素；临时选择恢复，坏 surface 不复用 |
| 算法 | 1×1、1×N、N×1、不同 radius/合法透明度 | 迁移前后输出逐字节一致，X 字节行为不变 |
| 缓存 | 同 origin / 改 origin / 捕获失败后重试 | 不重复刷新；失败不保留错误的缓存有效标志 |
| 消息 | 合规 owner draw、custom draw、DPI 载荷 | 一次解码、类型正确、未知通知仍走原默认逻辑 |
| 内核句柄 | 创建失败、跨线程 SetEvent、等待后 join、异常退出 | 关闭责任唯一，借用不越过所有者，协议行为不变 |
| 资源回归 | 同一进程反复创建/销毁四种窗口和表面 | 预热后句柄/GDI/USER 数量无按轮数持续增长的趋势 |

故障注入使用最窄的测试 seam：例如创建/销毁包装器中的 `#[cfg(test)]` 替换点，或编译期分派到小型测试实现。默认无额外生产逻辑，不引入运行时 trait object 和全项目 Win32 mock 框架。

测试状态使用局部或线程隔离机制，避免并行测试共享全局“下次调用失败”的标志。注入应模拟真实控制流结果，不伪造能够随意解引用的 native 指针。

### 13.3 Fatal 分支必须在子进程测试

不在普通单元测试进程中直接 abort。由父测试启动专用测试子进程：子进程确认到达注入点后留下 ready 标记；受保护状态的析构器若执行则写 freed 标记；注入销毁失败；父进程验证子进程异常退出、ready 存在且 freed 不存在。

同时测试正常分支的析构标记确实会写出，避免“测试根本没有到达目标”造成假通过。异常退出测试的故障开关只存在于测试构建/专用测试入口，不暴露为 release 应用环境变量或正常命令行功能。

### 13.4 交互式 Windows 测试单独执行

先查看现有 ignored 测试，再逐项运行确定安全且相关的过滤器：

```powershell
cargo test --locked -- --list
# 下行是命令模板；替换成实际存在的具体测试过滤器后执行。
cargo test --locked <具体测试过滤器> -- --ignored --test-threads=1
```

禁止盲目运行所有 ignored 测试或在未知桌面上模拟全局输入。优先使用隐藏窗口、离屏 DC 和合成像素 fixture；真实取色/滚轮/焦点场景在可用的本机桌面单独验证。涉及剪贴板时避免覆盖用户内容，无法可靠保存恢复则记录 NOT TESTED。

保留现有 mixed-DPI、负坐标、显示器间隙、小工作区、取色浮层不抢焦点、冻结高亮/点击一致性、快速复制与失败回退等回归项。Win11 实测不代表 Win10 实测通过。

非 Windows 环境仅运行实际可用的纯 Rust 验证，并记录平台；**不要求 Windows 本机为了跨平台检查启 Docker**。R05 应保证纯算法在非 Windows 构建中可编译，无法运行该环境时明确标注未执行。

### 13.5 性能与资源验证

复用项目现有 `examples/resource-probe.rs`、`docs/resource-probe.md`、`docs/performance-running-app.md` 和测量方法，不另造一套互不兼容的测量结果。[S15]

同一机器、相同构建配置、相同场景对比 baseline 和完成版本，记录空闲、Live、Frozen、结果窗和设置窗。关注线程数、句柄/GDI/USER 数、工作集、CPU、唤醒以及采样/绘制延迟。

先预热再做至少两组重复创建/销毁，检查数量是否随循环轮数持续增长。线程与私有 bitmap/DC 的稳定数量不得无说明增加；不要求不同时间的工作集逐字节相等。发生超出重复测量波动的回归时，定位新增分配、复制或原生调用，修复或在报告中明确阻塞，不预先虚构“无性能损耗”。

---

## 14. 完成标准、统计与交付

### 14.1 必须同时满足

1. R01 的生命周期与故障策略已落地，root/content/subclass 的关系及构造失败路径有测试证据。
2. 重复的 DC/bitmap/font owner 已收敛；每个自有资源只有一个释放责任；借用/stock 资源不释放。
3. DIB 布局、存储有效性和同步有集中契约；临时选择失败不会污染后续采样；没有新增实时路径分配或轮询。
4. 消息解码只在具契约的边界发生；没有无界生命周期引用、任意整数 safe 解码器或跨重入的可变状态借用。
5. 纯模糊算法保持输出一致，解除整个 ui 的平台 gate 时没有把 Windows 代码带到非 Windows 构建。
6. 输入事件的 owner/borrowed handle 适配完成，controller 继续禁止 unsafe，原协议测试保留。
7. lint 与当前环境可执行的验证通过；未测项、环境阻塞和既有失败准确列出。
8. 无未说明的功能/视觉/性能回归；未新增运行时依赖；未擅自运行 Docker、远程 CI 或发布操作。

### 14.2 unsafe 对比如何报告

新增 `docs/unsafe-refactoring-report-2026-09-27.zh-CN.md`，记录实际 `START_SHA` 和 `END_SHA`，将生产代码与测试/示例分开。

至少比较：词法 unsafe 命中（明确口径）、重复资源 Drop 实现数、裸消息解码的独立位置数、DIB 原始切片构造位置数、需要独立证明的生命周期边界，以及新增/迁移的测试。

如果使用 AST 统计，应给出脚本/工具版本与规则；不能把 grep 数称为精确块数。块数增加时解释是否来自拆小大块、显式调用契约或新增测试。**不得为了让数字下降撤回安全检查或合并大块。**

### 14.3 交付文件

- 实际代码与新增/迁移测试。
- `docs/unsafe-boundaries.md`：持续维护的安全边界与所有权约定。
- `docs/unsafe-refactoring-report-2026-09-27.zh-CN.md`：实施结果、偏离说明、命令/退出码、测试状态、资源/性能对比和未测项。
- 必要时更新 `docs/validation.md` 与相关说明的入口；不重写原始实现规划、不改写历史测量记录、不伪造历史 PASS。

报告使用 `PASS / FAIL / NOT TESTED / BLOCKED`，说明各状态对应的具体命令或场景。若由于环境不能完成验收，可准确交付“代码已修改，尚缺某项验证”，不得称全部完成。

最后进行一次专门的 diff 审查：检查所有新增 unsafe、所有 changed Drop、所有 `SetWindowLongPtrW`/subclass 引用、所有 `from_raw_parts*`、所有 `mem::forget`/原始句柄接管和所有 cfg 改动。新增 `unsafe impl Send/Sync`、`transmute`、`Box::from_raw` 或 `'static` 绕过生命周期时必须停下重新设计，而不是仅添加注释。

---

## 15. 可直接交给本地 agent 的执行提示词

```text
请先完整阅读 docs/unsafe-refactoring-plan-2026-09-27.zh-CN.md，以及项目实际存在的
AGENTS.md、本机环境说明和相关验证文档，然后在当前工作区完成这次重构。

先记录 HEAD、已有未提交修改、工具链与验证基线；如果当前代码比文档基线新，
逐项核对并保留已完成的修复，不回退、不覆盖用户修改。

按文档 R01—R07 和 P0—P7 实施。保留 Rust + Win32 + GDI、现有输入协议和低占用行为。
重点完成窗口销毁生命周期、GDI/DIB 资源复用、原生消息解码收敛、纯算法抽离及内核句柄类型化。
不要以 unsafe 关键字数量作为唯一目标，不新增通用框架、运行时依赖或无关功能。

Windows 本机直接执行原生测试，不使用 Docker；除非我另行明确要求，
不要推送、创建 PR、触发 GitHub CI、打 tag 或发布。遵循仓库的本地提交约定。

在本次任务中持续推进代码、测试、回归检查与实施报告，不只输出计划，
也不在每个阶段后停下来等待我确认。需要调整细节时选择满足安全契约且改动最小的实现，
并在报告中说明；不能以简化为由削弱文档列出的安全条件。

提交 docs/unsafe-boundaries.md 和
docs/unsafe-refactoring-report-2026-09-27.zh-CN.md。
测试状态和测量结果必须来自真实执行；不能运行的项目准确标为 NOT TESTED/BLOCKED。
最终说明完成内容、关键实现选择、验证结果、剩余限制和本地提交，不能把未验证内容写成 PASS。
```

---

## 16. 源码定位与官方依据

下列源码链接固定到核对基线，实施以本地当前 HEAD 为准。源码支持“当前实现在哪里、有哪些约束”；本文提出的结构和提交顺序是重构建议，不是官方要求。

### 固定基线源码

- [S01 — 基线提交](https://github.com/ghostroller/color-picker/commit/1a302eecd4d9954205cabff39186955e1bd6dcd3)
- [S02 — result.rs：callback、控件、subclass、析构、消息与绘制](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/result.rs)
- [S03 — settings.rs：设置窗状态、字体、子控件与消息](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/settings.rs)
- [S04 — preview.rs：稳定状态、重入顺序和现有销毁兜底](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/preview.rs)
- [S05 — capture.rs：GdiSampler、DIB、GdiFlush、临时选择](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/platform/windows/capture.rs)
- [S06 — drawing.rs：后备缓冲、字体、PaintSession](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/drawing.rs)
- [S07 — magnifier.rs：冻结浮层与绘制资源](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/magnifier.rs)
- [S08 — frost.rs：手写资源清理、缓存与 blur_and_tint](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/frost.rs)
- [S09 — input.rs：ControlSignal、线程等待和输入协议接入](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/platform/windows/input.rs)
- [S10 — theme.rs：字体、brush、custom draw 与 Paint 配对](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/theme.rs)
- [S11 — verify-windows.ps1：现有本机验证命令及 manifest/DPI 检查](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/scripts/verify-windows.ps1)
- [S12 — Cargo.toml：依赖、edition 和 release 配置](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/Cargo.toml)
- [S13 — lib.rs：ui 模块整体受 cfg(windows) 限制](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/lib.rs)
- [S14 — ui/mod.rs：Windows UI 模块注册](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/mod.rs)
- [S15 — resource-probe.md：已有资源测量文档入口](https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/docs/resource-probe.md)

### 官方接口与 Rust 依据

- [A01 — DestroyWindow：失败返回与线程/窗口树销毁契约](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-destroywindow)
- [A02 — WM_NCDESTROY：与子窗口销毁的顺序](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-ncdestroy)
- [A03 — SetWindowLongPtrW：零返回与 LastError 的解释](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowlongptrw)
- [A04 — CreateDIBSection：像素存储所有权与 GdiFlush 同步要求](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-createdibsection)
- [A05 — GetDC：获取的 DC 与 ReleaseDC 配对](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdc)
- [A06 — CreateCompatibleDC：MemoryDc 与 DeleteDC 配对](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-createcompatibledc)
- [A07 — OwnedHandle：所有权及 CloseHandle 限制](https://doc.rust-lang.org/std/os/windows/io/struct.OwnedHandle.html)
- [A08 — BorrowedHandle：与 owner 关联的借用生命周期](https://doc.rust-lang.org/std/os/windows/io/struct.BorrowedHandle.html)
- [A09 — Clippy undocumented_unsafe_blocks](https://rust-lang.github.io/rust-clippy/master/index.html#undocumented_unsafe_blocks)
- [A10 — slice::from_raw_parts：有效性、初始化、长度与生命周期条件](https://doc.rust-lang.org/std/slice/fn.from_raw_parts.html)

**执行原则：把不安全代码变得集中、短小、可证明；不要把复杂性藏起来，也不要为减少关键字牺牲原有行为。**

<!-- Reference definitions for inline source IDs. -->
[S01]: https://github.com/ghostroller/color-picker/commit/1a302eecd4d9954205cabff39186955e1bd6dcd3
[S02]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/result.rs
[S03]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/settings.rs
[S04]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/preview.rs
[S05]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/platform/windows/capture.rs
[S06]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/drawing.rs
[S07]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/magnifier.rs
[S08]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/frost.rs
[S09]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/platform/windows/input.rs
[S10]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/windows/theme.rs
[S11]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/scripts/verify-windows.ps1
[S12]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/Cargo.toml
[S13]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/lib.rs
[S14]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/src/ui/mod.rs
[S15]: https://github.com/ghostroller/color-picker/blob/1a302eecd4d9954205cabff39186955e1bd6dcd3/docs/resource-probe.md
[A01]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-destroywindow
[A02]: https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-ncdestroy
[A03]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowlongptrw
[A04]: https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-createdibsection
[A05]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdc
[A06]: https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-createcompatibledc
[A07]: https://doc.rust-lang.org/std/os/windows/io/struct.OwnedHandle.html
[A08]: https://doc.rust-lang.org/std/os/windows/io/struct.BorrowedHandle.html
[A09]: https://rust-lang.github.io/rust-clippy/master/index.html#undocumented_unsafe_blocks
[A10]: https://doc.rust-lang.org/std/slice/fn.from_raw_parts.html
