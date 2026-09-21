# 本机资源测量工具

实际 Release 进程的最新实测见[后台与取色性能](performance-running-app.md)。
已有诊断进程可用 `scripts/measure-running-app.ps1` 测自动启动/取消及静止 Live，
或用 `scripts/measure-passive-app.ps1` 只读观察用户手动打开的 Frozen / Result / Settings 状态。
后者支持等待目标状态，不会操作应用；具体命令和状态编号见性能记录。

`resource-probe` 是显式运行的 Windows 桌面测量工具。它使用本项目的 `PreviewController`，每轮建立真实会话输入钩子，进入 Live、采样并提交一帧预览，然后主动取消，保持消息循环直到输入线程退出并被回收。工具不创建托盘、不注册快捷键、不生成键鼠输入、不修改剪贴板，也不保存屏幕图像。

运行前通过托盘退出已有 `color-picker`。测量期间保持桌面解锁、显示设置稳定，不操作鼠标按钮或 Esc；短暂会话会接管真实鼠标点击/滚轮及 Esc。工具与主程序共用单实例保护，已有实例时拒绝测量。启动时已有按钮按住、会话错误或超时会输出 `NOT_COMPLETED`，不会把不完整测量算作成功。错误路径仍请求取消并等待输入释放和线程回收；检查报告的 `cleanup_completed` 与 `cleanup_error`。

```powershell
cargo build --release --locked --example resource-probe
New-Item -ItemType Directory -Path .\logs\measurements -Force | Out-Null
# 默认：20 轮预热 + 100 轮测量，前后各静置 10 秒。
.\target\release\examples\resource-probe.exe --output .\logs\measurements\resource-probe-100.json

# 较长的同机参考测量：前后各静置 60 秒，记录 100/500/1000 轮检查点。
.\target\release\examples\resource-probe.exe --cycles 1000 --idle-seconds 60 --output .\logs\measurements\resource-probe-1000.json
```

输出目录必须存在，输出文件必须是新文件，避免覆盖旧证据。省略 `--output` 时 JSON 写入标准输出，进度写入标准错误。`--cycles` 范围为 1–10000，`--idle-seconds` 为 1–3600。总轮数包括另外执行的固定 20 轮预热。工具创建隐藏宿主窗口，并在测量线程设置可恢复的 PMv2 上下文；示例测量不能代替主 EXE 的 manifest 验证。

报告包含工作集、私有提交、线程数、句柄数、GDI/USER 对象数，冷态/使用后 Idle 的内核与用户 CPU 时间增量，以及当前控制器 Idle/定时器/输入线程所有权状态。资源采样来自当前进程，包括测量宿主、JSON 输出句柄和少量工具开销；工作集与私有提交分别记录。线程总数可能包含系统创建的线程，不能把它解释为应用自行创建的工作线程数。工具不自动清空工作集或调整计时器精度。

空闲 CPU 增量使用单独的 GetProcessTimes 采样包围阻塞消息等待，排除前后资源统计自身的开销；
因此不必等于 `before/after` 资源快照中累计 CPU 字段的差。显示 0 表示 API 计时粒度内未观察到增量，
不是绝对零执行成本。线程枚举等探针工作可能增加资源快照里的累计内核时间。

启动延迟从调用 `PreviewController::start` 开始，直到首次有效 Live 采样及本进程预览窗口 `UpdateWindow` 返回，按 nearest-rank 计算 p50/p95，原始样本保留在 JSON。它不包含真实快捷键投递，也不代表显示器已经呈现画面的时间。冷态资源在预热前采集；延迟只统计预热后的成功轮次。

`COMPLETED` 只表示该次受控“启动 → 一帧 Live → 取消 → 正常回收”的测量完成，不是性能阈值通过声明。本工具不测真实完整点击手势、冻结静止/冻结延迟、结果与设置窗口、复制流程或干净机器部署。它尤其不能替代完整 1000 次取色/放大/复制工作流、混合 DPI、权限和发布矩阵验收；这些仍需在验证文档单独记录。JSON 中会列出未测范围，失败/部分完成的检查点保留以便调查。
