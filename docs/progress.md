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
