# 验证记录

## 环境 · 2026-09-20

- Windows x64，Build 26200.9457 / DisplayVersion 25H2（注册表 ProductName 仍为 Windows 10 Enterprise）。
- `rustc 1.95.0 (59807616e 2026-04-14)`，LLVM 22.1.2。
- `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`。
- Visual Studio 2022 Build Tools，MSVC 14.39.33519。
- Windows SDK 可用资源编译器：`10.0.22621.0/x64/rc.exe`，文件版本 10.0.22621.2428。
- 构建目标：`x86_64-pc-windows-msvc`；静态 CRT。

## M0 检查

检查以 M0 阶段提交中的代码为准；后续复测另行追加。

| 检查 | 结果 | 证据 |
|---|---|---|
| Debug Windows 构建 | PASS | `cargo build --locked` |
| 运行时 DPI | PASS | 最小 EXE 比较线程 DPI context 与 PMv2 相等 |
| 格式与 Clippy | PASS | `cargo fmt --all -- --check`；`cargo clippy --all-targets --locked -- -D warnings` |
| 核心测试 | PASS | 39 项：颜色/格式 8、矩形 5、状态 13、放大映射 13 |
| Release Windows 构建 | PASS | `cargo build --release --locked --target x86_64-pc-windows-msvc` |
| EXE 实际嵌入 manifest | PASS | SDK 10.0.22621.0 的 mt.exe 提取资源 #1，检查 PMv2/asInvoker/Common-Controls 6 |
| 完整验证脚本 | PASS | `scripts/verify-windows.ps1` 全部完成，Release 运行时 DPI 检查通过 |
| Linux 纯逻辑测试 | NOT TESTED | 已配置 CI，当前机器未执行 Linux 作业 |

## M1 检查 · 2026-09-20

基于 `5044e3c` 之后的 M1 阶段代码（对应 `feat(windows): add M1 resident shell and desktop smoke tests` 提交）。

| 检查 | 结果 | 证据 / 边界 |
|---|---|---|
| 格式、Clippy、默认测试、Release、manifest、运行时 DPI | PASS | `scripts/verify-windows.ps1`；40 项默认测试，桌面测试默认忽略 |
| 隐藏的顶层宿主 | PASS | 桌面测试确认不可见、无 parent、无 WS_CHILD |
| 单实例与重复启动 | PASS | 二次进程退出码 0，原实例激活 +1，唯一宿主，mutex 返回 Existing |
| 默认热键注册及释放 | PASS | 注册成功；退出后新实例可再次注册 |
| WM_HOTKEY 路由 | PASS | 向自有测试窗口发送消息，激活计数恰好 +1；不是实际键盘输入测试 |
| 受控热键冲突 | PASS | 测试预先占用 Ctrl+Alt+C；应用保持托盘并从托盘通知激活 |
| 托盘恢复处理 | PASS | 仅向自有宿主发送 TaskbarCreated，重建成功；未重启 Explorer |
| 退出 / 重启 | PASS | WM_CLOSE 后正常退出，宿主消失，再启动成功 |
| EXE 动态依赖初查 | PASS | MSVC dumpbin /dependents 未列出 VCRUNTIME / MSVCP / UCRT DLL；不能替代干净机器验收 |
| 待机结构 | PASS（代码检查） | 阻塞 GetMessage；无 SetTimer / hooks / 应用线程创建 / GDI 采样 / 网络调用 |
| 实际快捷键、重复按住与通知可见性 | NOT TESTED | 自动测试不生成输入；系统通知策略可能抑制提示 |
| 托盘菜单实际点击与键盘导航 | NOT TESTED | 自动测试不打开用户菜单 |
| Explorer 真实重启、锁屏、电源、显示变化 | NOT TESTED | M1 只验证接线，M6 再验证完整采样恢复行为 |
| 长时间 CPU / 句柄 / 内存测量 | NOT TESTED | M7 执行，不以结构检查代替测量 |

桌面测试命令：

```powershell
cargo test --locked --test windows_shell -- --ignored --test-threads=1 --nocapture
```

首次在受限沙箱桌面执行失败：`Shell_NotifyIconW(NIM_ADD)` 返回失败，应用未就绪。
在当前用户交互桌面执行后，2 项均 PASS。测试已正常关闭自己的全部临时进程；
没有操作用户已有的 color-picker、重启 Explorer、生成键鼠输入或改变剪贴板。

2026-09-20 用户补充实测：日志 `color-picker-20260920-213810-fdaa2d1e81304c9bb11c4877854535fe.log`
与用户“正常触发了”的反馈确认实际快捷键和通知显示 PASS。进程 34612 的 elapsed_ms=8930/12464
均有 hotkey.received → activation.handled → tray.notification_accepted → tray.balloon_show；
48916 有 tray.activation_received，52365 有设置通知和 Shell 显示回调。无错误事件。
继续读取完整日志还确认 56305ms 菜单开始、59423ms 菜单退出、59436ms 宿主资源释放及成功退出。
因此实际入口的可见效果、托盘激活、设置和退出菜单 PASS；长按不重复触发和真实 Explorer
重启仍为 NOT TESTED。这些剩余项继续留在发布前回归中，
M1 阶段表的进入下一阶段条件已经满足，可进入 M2。

补充诊断验证（M1 日志变更）：2026-09-20，设置 `CARGO_TARGET_DIR=target/diagnostic` 后
完整运行 `scripts/verify-windows.ps1`，51 项默认测试及其余检查 PASS。另执行 Release
`--check-environment --log-file <含中文和空格的绝对路径>`，确认日志含 `app.start`、
`environment.pmv2_ok`、`app.exit status=success`；缺失日志路径返回 2。启动脚本语法检查 PASS。
诊断变更后的两项交互桌面测试为 NOT TESTED（已有用户实例，未关闭或接管）；前面的桌面 PASS
仍仅适用于原 M1 提交。后续用户日志确认正常触发，未复现先前的“无可见效果”。

## M2 检查 · 2026-09-20

| 检查 | 结果 | 证据 / 边界 |
|---|---|---|
| 格式、Clippy、默认测试、Release、manifest、运行时 DPI | PASS | `CARGO_TARGET_DIR=target/m2`，完整验证脚本；65 项默认测试通过，桌面测试默认忽略 |
| 独立像素窗口 PMv2 | PASS | 显式给 example 嵌入 manifest，运行 `pixel-fixture --check-environment`；不靠 DPI override 掩盖缺失资源 |
| 当前桌面已知像素 | PASS | 8 个公式像素、3 个单像素 RGB 条纹；窗口物理原点 (60,100)，采样器逐点一致 |
| 鼠标位置不变、画面变化 | PASS（采样器） | 在相同物理坐标改变测试窗口像素并重新捕获；主控制器每个 timer 都采样，不以坐标变化为条件 |
| 预览位置与非激活 | PASS（当前桌面） | 窗口位于工作区且不覆盖采样点，检查实际矩形及样式，显示 / 更新前后前台窗口不变 |
| 不变内容不重绘 | PASS | 同一颜色和坐标无新增 update region；颜色 / 不可采样状态变化和隐藏重显有正确更新 |
| 隐藏旧提示后采样 | PASS（受控窗口） | 关闭捕获排除标志后，隐藏与刷新 DWM，仍准确读到旧提示位置下方的已知像素 |
| 采样器资源 | PASS（100 次） | 预热后创建 / 采样 / 销毁 100 次，GDI 对象 2→2 |
| 预览资源 | PASS（100 次） | 预热后 GDI / USER 3 / 2；25 次及 100 次后均为 3 / 2；不是完整取色会话 1000 次验收 |
| 启停、重复激活、旧消息 | PASS | 宿主测试验证重复激活不新建会话，停止后 timer=0 且采样计数不再增长，重开代际递增；旧 timer 不影响新会话或 Idle |
| 菜单期间积压激活 | PASS（调度回归） | 模拟活动菜单中收到重复激活、随后停止、最后消费队列的时序；3 个入口均保持 Idle，新的停止后请求仍可启动 |
| 显示变化停止 | PASS（消息接线） | 仅向测试宿主发送 WM_DISPLAYCHANGE，活动会话停止且不被旧 timer 恢复；未改变真实显示配置 |
| 单实例、热键冲突、托盘恢复 | PASS | M2 版本重跑 2 项宿主测试；不重启 Explorer、不合成键鼠输入、不操作剪贴板 |
| 负坐标、工作区、边角、显示空隙 | PASS（纯逻辑） | 定位函数覆盖负坐标、极限整数、四角 / 任务栏、穷举小区域；监视器查找保留显示空隙 |
| 跨 DPI / 负坐标副屏 / 实际鼠标跨屏 | NOT TESTED | 实机准入未完成，不能由 PMv2 manifest 或纯逻辑测试推断通过 |

桌面测试命令（在已解锁、稳定的交互桌面执行，先退出已有 color-picker）：

```powershell
cargo test --locked --test windows_capture --test windows_preview --test windows_shell -- --ignored --test-threads=1 --nocapture
```

5 项测试全部通过，测试自行关闭临时窗口和进程。受限沙箱内真实捕获曾返回访问拒绝，
使用当前用户的交互桌面复测通过。上述 PASS 限于本次受控窗口、显示配置和消息测试；
原定完成真实混合 DPI 与负坐标副屏验收后进入 M3；用户随后明确授权直接实现到 M5、减少测试，
这些项目继续如实保留未测，不阻塞此次实现。

## M3–M5 必要验证 · 2026-09-20

用户授权直接实现到 M5，减少测试；此次不等待完整多屏矩阵或执行压力测试。

| 检查 | 结果 | 证据 / 边界 |
|---|---|---|
| 最终构建验证 | PASS | `CARGO_TARGET_DIR=target/m2`，完整 `scripts/verify-windows.ps1`：格式、全目标 Clippy、72 项默认测试、Release、实际 manifest 与应用 / 示例 PMv2 |
| 输入配对协议 | PASS（逻辑） | 部分安装不拦截、完整 down/up、候选等待 / 拒绝、Esc 重复、右键取消、多按钮释放；不等于实际不同程序的穿透矩阵 |
| 实际线程 / 钩子生命周期 | PASS（宿主冒烟） | 两项既有 Windows 宿主测试在 M5 版本通过，异步等到 Live，再停止 / 重开 / 显示变化取消；停止后无采样、旧 timer 不串会话 |
| 冻结快照准确性 | PASS（受控窗口） | 原采样器测试增补 65×65 全像素通道 / 行序、超过范围拒绝、桌面像素变化后快照不变、截图后 1×1 继续采样 |
| 放大镜与结果控件 | PASS（单次桌面冒烟） | 4/8/16/32 倍缓存命中与锚点、固定窗口、无效文字栏、缩回 Live 信号；4 个原生只读 EDIT 文本与 formatter 一致、关闭后 HWND 销毁 |
| 剪贴板内存及重试 | PASS（局部） | GlobalAlloc/Lock/最终 Unlock；最多 3 次重试、新复制 / 关闭使旧 token 失效；未写用户剪贴板 |
| 实际按键选择 / 鼠标滚轮 / 复制粘贴全流程 | NOT TESTED | 此次未合成输入或改剪贴板，留待用户体验反馈 |
| 混合 DPI、多屏负坐标、长时间资源 / 延迟、1000 次会话 | NOT TESTED | 按用户要求留待 M6–M7 / 发布前验收 |

仅执行一次以下短桌面冒烟（约 3 秒，4 项通过；测试关闭自己的全部临时窗口 / 进程）：

```powershell
cargo test --locked --test windows_capture --test windows_selection_ui --test windows_shell -- --ignored --test-threads=1 --nocapture
```

M2 旧预览资源压力测试本次未重复执行。最终构建为
`target/m2/x86_64-pc-windows-msvc/release/color-picker.exe`；日常启动脚本会在独立 diagnostic 目录重新构建当前版本。

## M6 必要验证 · 2026-09-20

用户对 M3–M5 实测反馈“看起来没问题”，作为体验确认记录；未由此推断全部矩阵通过。

| 检查 | 结果 | 证据 / 边界 |
|---|---|---|
| 构建与默认检查 | PASS | 完整 verify-windows.ps1：格式、全目标 Clippy、76 项默认测试、Release、实际 manifest、主程序与像素示例 PMv2 |
| 配置严格校验与保护 | PASS | 4 项隔离文件测试，包含重载一致、无效键/字段/枚举、损坏及未来版本保留、替换失败保留旧文件并清临时文件 |
| 热键事务回滚 | PASS（受控 Win32） | 1 个显式 ignored 测试，自己的 HWND、临时配置及 Ctrl+Alt+Shift+F9/F10/F11；冲突保留旧键，保存失败释放临时键，成功换键后按退役顺序释放 |
| 设置与结果控件 | PASS | 复用一次原生窗口冒烟，配置草稿往返、无 Ctrl/Alt 拒绝、只读禁止应用、HSL 默认按钮文本；未改剪贴板 |
| 宿主与恢复消息 | PASS（接线） | 2 项短宿主测试，真实托盘/钩子启停、冲突仍可托盘取色、显示变化消息取消、模拟 TaskbarCreated 恢复 |
| 设置阻断、退出/失效优先级 | PASS（代码审查与已有状态测试） | 设置禁止激活；先取消 Finishing 候选再处理线程完成，防止旧结果自动复制；同批取消阻止“重新取色” |
| 真实修改设置后重启、自动复制粘贴 | NOT TESTED | 配置重读/默认按钮已验证，完整用户 UI 与真实剪贴板流程未自动操作 |
| 锁屏/休眠/RDP/显示器拔插 | NOT TESTED | 本次不改变真实桌面环境，恢复接线不代表实机通过 |

显式桌面验证仅一次，4 项全部通过，测试清理自己的窗口、临时热键/文件和进程。

## M7 本机测量与预览包 · 2026-09-20

Windows 11 x64 25H2 / 26200.9457，Rust 1.95.0，静态 CRT Release；
完整原始数据见 [1000 次启动/取消报告](measurements/windows11-start-cancel-1000.json)。
固定预热 20 次，随后 1000 次各采样并提交一帧再取消，前后各空闲 60 秒，
总耗时约 133.29 秒；COMPLETED，cleanup_completed=true。

| 采样点 | 工作集 MiB | 私有提交 MiB | 句柄 | GDI / USER | 线程 |
|---|---:|---:|---:|---:|---:|
| 冷态空闲 60 秒后 | 9.86 | 1.41 | 142 | 0 / 3 | 1 |
| 预热 20 次后 | 11.13 | 1.79 | 145 | 1 / 3 | 2 |
| 测量 100 次后 | 11.16 | 1.79 | 145 | 1 / 3 | 2 |
| 测量 500 次后 | 11.18 | 1.79 | 145 | 1 / 3 | 2 |
| 测量 1000 次后 | 11.29 | 1.81 | 145 | 1 / 3 | 2 |
| 使用后空闲 60 秒后 | 11.27 | 1.68 | 145 | 1 / 3 | 1 |

所有检查点 controller=Idle、timer=0、input_worker_attached=false；预热后句柄、GDI/USER
数量保持不变。1000 次后相对预热的私有提交增加 28 KiB，工作集增加 160 KiB，
空闲后均回落；没有观察到句柄或 GDI/USER 随会话数增长。

受控 `PreviewController::start` 至有效采样及 WM_PAINT 提交：p50 **11.43 ms**、
p95 **12.33 ms**，原始 1000 个样本已保留。该数值不含宿主热键接收及前置合成同步，
不是用户看见画面的延迟，也不代替完整热键唤起 100ms 预算验收。
冷态和使用后两个 60 秒等待区间的 CPU 内核/用户增量均为 0ms（GetProcessTimes 计时粒度内）；
该区间排除资源枚举本身，因此与资源快照累计 CPU 的前后差不同。

工具未创建托盘/注册热键，数字包含测量宿主和输出文件开销，不能当作应用完整进程的固定内存承诺。
Frozen 60 秒静止、冻结延迟、动态 Live、包含结果/复制的完整 1000 次流程、干净机器仍为 NOT TESTED。
Frozen 转换现销毁 Live 预览窗口及其 GDI 缓存，返回实时再重建；该优化不改变缓存颜色和源坐标。

打包使用锁定依赖和 x64 Release；包含原始依赖许可证、Rust 库版权、Cargo.lock、
源码提交/脏状态/工具链/EXE 哈希及 ZIP SHA256。M7 当前交付为本地预览包，
未签名，项目许可未指定，完整 v0.1 发布验收尚未完成。

M7 最终源码再次通过完整 `verify-windows.ps1`（76 项默认测试、Clippy、Release、
manifest 及 PMv2）；未重复桌面冒烟。打包脚本实际运行并解压验证：110 个文件，
包含 46 个依赖的许可目录和 Rust 库许可目录；ZIP 校验值与解压 EXE 校验值一致，
解压后的 EXE `--check-environment` 成功。EXE 为 **475136 bytes（464 KiB）**，
SHA256 `6e339b5417c47a464332ea83587f65d5b141ee2547a2b145159962a43eff9f40`。
实际 PE 依赖未列出 VCRUNTIME/MSVCP/UCRT DLL；只验证本机，不代替干净机器测试。
交付包在阶段提交后重新生成，使 `build-info.json` 的 source_commit 对应干净源码。

## 发布前实机矩阵

2026-09-21 原生界面更新已完成 175% 缩放下的实际窗口检查、已有控件冒烟与
标准构建验证，截图与独立预览命令见 [界面预览](ui-preview.md)。

同日按用户反馈加入主键点击监听、冻结窗外左键取消，以及紧凑取色浮窗。
按键录入、非法键、长按、Tab / Esc 与焦点切换已通过自有窗口消息测试，
不发送系统输入、不写用户配置或剪贴板；窗外 / 窗内无效区域规则由纯逻辑测试覆盖。
更新后的默认测试共 77 项通过。实际鼠标点击的全流程仍由实机矩阵记录。

随后进一步取消取色区边距：实时浮窗缩为 168×38 DIP，色块贴边；冻结视口按缓存
初始 4× 的物理像素范围收紧，信息栏缩为 24 DIP。175% 下完整与边缘窄快照均已
检查实际截图。布局、最高倍率最小空间与选中框裁剪测试通过；完整标准检查的
80 项默认测试及原生控件 / 冻结倍率映射冒烟通过。未重复长时间性能测量。

磨砂与边框更新：175% 下检查实时、完整冻结与边缘窄快照的真实窗口截图，
信息区约 16% 的背景参与混合，色块保持原 RGB。非激活窗口的系统 Acrylic 在本机
未呈现透底效果，因此使用局部背景模糊缓存，不更改系统透明设置。
独立合成棋盘探针确认：仅 SRCCOPY 不能排除自身；设置 WDA_EXCLUDEFROMCAPTURE 后，
普通 layered 窗下的 SRCCOPY / CAPTUREBLT 均得到正确背景。设置失败时回退纯色。
标准检查 81 项默认测试、原生控件与全倍率冒烟通过；100 次创建 / 销毁检查中
GDI / USER 为 2 / 2，预热、第 25 次和第 100 次一致。没有新增后台计时器；
移动时增加局部背景捕获与线性模糊，尚未重做长期性能测量。
隐藏浮窗后的准确采样复测通过。首次失败源于测试图案窗口仅在创建后设置置顶，
在当前桌面被其他窗口遮挡；测试 fixture 同样改为创建时指定置顶后恢复通过，
未修改生产采样路径或系统设置。

随后按用户反馈调整视觉参数：下、右边框从 1 物理像素加粗至 2 DIP（175% 为 4 物理像素），信息区背景混合占比从约 16% 提高至约 35%（深色约 65%）。

外观偏好：设置页新增边框粗细（0–6 DIP）与背景透明度（0–80%）两个滑块，
实时更新数值，只在“应用”后保存并影响下一次取色。旧配置自动补默认 2 DIP / 35%。
完整标准检查的 83 项默认测试通过，覆盖旧配置兼容、新值重载和非法值保护；
原生控件冒烟通过，覆盖滑块初值 / 范围、方向键 / End、Tab、数字刷新、应用草稿、
关闭与禁止保存。175% 下检查设置页、默认外观与 4 DIP / 60% 外观截图。
粗边框覆盖的冻结像素列拒绝命中，原始颜色缓存与整数映射保持不变。

本轮预览冒烟首次发现动态 `SetWindowPos(HWND_TOPMOST)` 返回成功但置顶标志未设置；
独立原生 STATIC 窗口亦复现，与本次尺寸修改无关。两种浮窗现在创建时即指定
`WS_EX_TOPMOST`，未加入轮询、未改变系统设置。复测原断言通过，且 100 次预览
创建 / 销毁后的 GDI / USER 为 0 / 2，与预热基线及第 25 次一致；非激活检查通过。

2026-09-20 补充实际常驻程序的后台/前台性能测量，见 [性能记录](performance-running-app.md)。
该记录使用正在运行的诊断 Release，覆盖 30 秒 Idle/Live/返回 Idle、20 次自动调用及
已有真实热键日志，区别于 M7 的控制器内启动/取消探针。

当前实现到 M6，并完成 M7 工具和本机启动/取消测量。下列是端到端发布矩阵；
局部测试通过不等于对应完整场景已通过。

| 编号 | 场景 | 结果 |
|---|---|---|
| W01 | 单屏 100% 精确像素 | NOT TESTED |
| W02 | 125% / 150% / 200% | NOT TESTED |
| W03 | 混合 DPI 跨屏 | NOT TESTED |
| W04 | 左侧 / 上侧负坐标副屏 | NOT TESTED |
| W05 | 显示器间空隙 | NOT TESTED |
| W06 | 边缘 / 角落 / 任务栏 | NOT TESTED |
| W07 | 点击 / 滚轮不穿透 | NOT TESTED |
| W08 | 动态画面冻结 | NOT TESTED |
| W09 | 旧提示窗口自污染 | 部分：M2 受控隐藏路径 PASS；跨屏动态移动待测 |
| W10 | 全倍率与格边界实机命中 | 部分：缓存倍率 / 控件冒烟 PASS；实际鼠标全边界待测 |
| W11 | 放大镜外 / 文字栏 / 留白点击 | NOT TESTED |
| W12 | 按住启动 / 长按 / 双击 / Esc | NOT TESTED |
| W13 | 快速启停会话隔离 | 部分：M5 timer / 钩子生命周期冒烟 PASS；真实快速输入待测 |
| W14 | 配置冲突 / 损坏 / 保存失败 | PASS（隔离文件与真实 Win32 热键事务）；真实用户 UI 操作重启待测 |
| W15 | 剪贴板占用 | NOT TESTED |
| W16 | Explorer 重启 | NOT TESTED |
| W17 | 锁屏 / 休眠恢复 | NOT TESTED |
| W18 | 显示拓扑变化 / RDP 断开 | NOT TESTED |
| W19 | 管理员 / 普通程序 / 系统界面 | NOT TESTED |
| W20 | HDR / 颜色滤镜 | NOT TESTED |
| W21 | 1000 次会话资源回归 | 部分：1000 次启动/首帧/取消资源测量完成；完整取色/冻结/结果/复制流程 NOT TESTED |
| W22 | 干净 Windows 机器 | NOT TESTED |
