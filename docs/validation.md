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

## 发布前实机矩阵

M0 不含桌面取色功能，下列项目均尚未执行，不代表支持已经验证。

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
