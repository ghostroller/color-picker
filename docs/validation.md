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

## 发布前实机矩阵

当前有实时预览，完整输入与结果流程尚未实现。下列是端到端发布矩阵；
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
| W10 | 全倍率与格边界实机命中 | NOT TESTED |
| W11 | 放大镜外 / 文字栏 / 留白点击 | NOT TESTED |
| W12 | 按住启动 / 长按 / 双击 / Esc | NOT TESTED |
| W13 | 快速启停会话隔离 | 部分：M2 timer / 会话隔离 PASS；完整输入流程待测 |
| W14 | 配置冲突 / 损坏 / 保存失败 | NOT TESTED |
| W15 | 剪贴板占用 | NOT TESTED |
| W16 | Explorer 重启 | NOT TESTED |
| W17 | 锁屏 / 休眠恢复 | NOT TESTED |
| W18 | 显示拓扑变化 / RDP 断开 | NOT TESTED |
| W19 | 管理员 / 普通程序 / 系统界面 | NOT TESTED |
| W20 | HDR / 颜色滤镜 | NOT TESTED |
| W21 | 1000 次会话资源回归 | NOT TESTED |
| W22 | 干净 Windows 机器 | NOT TESTED |
