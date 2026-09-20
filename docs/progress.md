# 开发记录

按 [实现计划](implementation-plan.md) 的验收门槛推进，不以代码存在代替实机验证。

## 2026-09-20 · 仓库初始化

- 在已有计划的目录初始化 Git `main` 分支。
- 初始提交 `8a31ce4` 保存原始实现计划。
- 正式计划保存在 `docs/implementation-plan.md`；根目录文件保留跳转入口。

## M0 · 工程与纯逻辑

状态：**已通过本阶段验收**。Windows Debug/Release 构建、39 项纯逻辑测试、
Clippy、格式检查、EXE manifest 提取及运行时 PMv2 检查通过。核心禁止 unsafe，未导入 Windows API。

实现：

- Rust 2024 工程、精确版本 `windows = 0.62.2` / `embed-resource = 3.0.11`、锁文件、静态 CRT 和 Release 配置。
- 必需的 PMv2 / asInvoker / Common-Controls 6 manifest；构建失败不能静默跳过资源。
- 纯 Rust 颜色、HEX/RGB/CSS RGB/HSL 格式、物理像素矩形和 65×65 冻结裁剪。
- BGRX 快照访问、整数放大格命中、倍率锚点、留白与负坐标处理。
- 带会话代际的状态机；资源和输入都就绪才开始，清理结束才产生结果。
- Windows 验证脚本、Windows / Linux CI 定义；尚未声称远程 CI 已运行。

验证命令与实际结果见 [验证记录](validation.md)。主要文件为 `src/core/`、`tests/`、
`resources/`、`build.rs`、`scripts/verify-windows.ps1`。

后续阶段的输入协议、配置、GDI 和 UI 测试尚未实现，不能由 M0 测试推断通过。

阶段提交：`5044e3c`。

## M1 · 常驻外壳

状态：**实现与自动化验收通过，人工入口验收待完成**。
40 项默认测试和 2 项显式桌面冒烟测试通过；实际键盘触发、托盘菜单操作与真实 Explorer
重启尚未执行，因此尚未将 M1 的全部实机验收标记完成，也未越过该门槛开始 M2。

已实现：

- 隐藏的普通顶层宿主、阻塞消息循环，单独处理 GetMessage 的 -1 / 0。
- 托盘 Version 4、三个菜单项、阶段提示和 TaskbarCreated 重建；重建失败有序退出，
  避免留下没有托盘入口的隐藏进程。
- 当前登录 / Windows 会话范围的单实例互斥对象；二次启动有限重试向原宿主发送激活。
- 默认 Ctrl+Alt+C、MOD_NOREPEAT；冲突时提示且保留托盘入口。
- WTS 通知注册 / 注销；显示、会话、电源、系统退出消息接入。
- 回调只写入主线程 Cell 待办，离开回调后执行 Win32 副作用；不持有跨重入的应用可变借用。
- 热键、会话通知、托盘先释放，再销毁宿主与类，最后释放单实例句柄。
- 独立、默认忽略的 Windows 桌面冒烟测试；诊断查询不创建周期性工作。

当前激活只显示 M1 提示，设置只显示阶段说明，这是计划规定的阶段入口。
此阶段未安装 timer/hook、未创建应用工作线程、未采样屏幕、未做网络 I/O。

验收状态和待补人工项见 [验证记录](validation.md)。M2–M7 尚未开始；下一阶段从
M1 人工入口验收继续，再接 GDI 实时采样和非激活提示窗口。
