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

进入 M2 前剩余人工验证：启动 Release EXE，确认托盘出现；按 Ctrl+Alt+C 显示阶段提示，
按住不重复触发；通过托盘“开始取色”激活、“设置”查看阶段说明、“退出”结束进程。
系统允许显示通知时检查可见效果，再记录 PASS/FAIL。此处不把尚未执行的步骤记为完成。

## 发布前实机矩阵

当前阶段不含桌面取色功能，下列完整发布场景均尚未执行，不代表支持已经验证。

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
| W09 | 旧提示窗口自污染 | NOT TESTED |
| W10 | 全倍率与格边界实机命中 | NOT TESTED |
| W11 | 放大镜外 / 文字栏 / 留白点击 | NOT TESTED |
| W12 | 按住启动 / 长按 / 双击 / Esc | NOT TESTED |
| W13 | 快速启停会话隔离 | NOT TESTED |
| W14 | 配置冲突 / 损坏 / 保存失败 | NOT TESTED |
| W15 | 剪贴板占用 | NOT TESTED |
| W16 | Explorer 重启 | NOT TESTED |
| W17 | 锁屏 / 休眠恢复 | NOT TESTED |
| W18 | 显示拓扑变化 / RDP 断开 | NOT TESTED |
| W19 | 管理员 / 普通程序 / 系统界面 | NOT TESTED |
| W20 | HDR / 颜色滤镜 | NOT TESTED |
| W21 | 1000 次会话资源回归 | NOT TESTED |
| W22 | 干净 Windows 机器 | NOT TESTED |
