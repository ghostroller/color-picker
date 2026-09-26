# unsafe 边界重构实施报告 · 2026-09-27

本报告对应 [实施计划](unsafe-refactoring-plan-2026-09-27.zh-CN.md) 的 R01—R07 / P0—P7。
代码、安全边界说明与新增测试已落地；以下状态只覆盖实际记录的构建和执行。
最终完整 Windows 验证已通过：168 项默认测试、全目标严格 Clippy、MSVC Release、实际 manifest 与应用/夹具 PMv2。
**逐项复核还确认：资源构造失败矩阵和未知通知默认分发链的动态覆盖未补齐，属于本机可继续完成的测试工作。
基线与完成版本的性能比较仍为 BLOCKED；不能据此宣称第 14 节全部验收完成。**

## 1. 源码、工作区与环境

| 项目 | 实际记录 |
|---|---|
| START_SHA | `1a302eecd4d9954205cabff39186955e1bd6dcd3` |
| END_SHA | `f5e4eccd81a80f8c8a4896f22e1bd6515361fd2d`（实现提交，见下方文档提交边界） |
| 分支 | 从初始 `main` 建立本地 `codex/unsafe-refactoring-20260927`；未推送 |
| 初始未提交内容 | 只有用户提供的未跟踪文件 `docs/unsafe-refactoring-plan-2026-09-27.zh-CN.md`；保留原文 |
| 项目约定 | 检查仓库及上级目录，未发现适用的 `AGENTS.md`；沿用仓库验证脚本与本地提交约定 |
| 操作系统 | Windows 11 x64，25H2，Build `26200.9457`；注册表 ProductName 的旧名称不作为 Windows 10 验收证据 |
| Rust / Cargo | `rustc 1.95.0 (59807616e 2026-04-14)` / `cargo 1.95.0 (f2d3ce0bd 2026-03-21)` |
| 原生工具链 | 本机 MSVC x64 工具链；Windows SDK `10.0.22621.0` |
| 构建配置 | `x86_64-pc-windows-msvc`、仓库现有静态 CRT 与 Release 配置、锁定依赖 |
| 执行边界 | Windows 本机原生执行；桌面访问受限的检查改在获准的本机桌面执行；没有 Docker、推送、GitHub CI、发布或 tag 操作 |

开工时 HEAD 与计划源码基线一致；现有字体、语言、冻结坐标、边缘布局、复制反馈和输入协议修复均在当前实现上继续保留，没有回退到旧实现。
未新增运行时依赖。已有用户常驻实例始终保留，没有发送退出命令、结束其进程或修改其配置。

初始 `scripts/verify-windows.ps1` 实际工具返回退出码 `0`，并运行到末尾成功信息，见
[基线验证日志](../logs/unsafe-refactoring/baseline-verify.log)。该日志未另存退出码文件，
也未保存测试 stdout 的完整计数，因此不从这份日志补造基线测试数量。

## 2. 实现与阶段对应

| 阶段 / 要求 | 已实施的具体改变 | 验证或剩余条件 |
|---|---|---|
| P0 | 核对 HEAD、未提交修改、环境；运行基础验证，保存基线二进制并尝试原有资源探针；生成生产/测试/示例分开的清单 | 基础验证有实际日志；性能基线 BLOCKED，见第 6 节 |
| P1 / R01 | `WindowLifetime` 统一跟踪根窗口；root/content 角色分离；稳定 Box 仅由 Rust owner 拥有；根结束后才释放 callback、字体、图标；构造失败走同一清理路径 | 原生树、部分构造、外部先销毁、content 单独结束、fatal 子进程与 BeginPaint 重入测试均已执行 |
| P2 / R02 | 公共 `gdi.rs` 管理 DC、bitmap、font、brush、pen、paint session 和保存的 DC 状态；旧重复资源 Drop 收敛；`BitmapDc` 管理选择关系和失效 | 已有 DC/bitmap 构造、选择/恢复失败、双重失败保留、字体恢复失败后的换代与析构测试 PASS；尚未覆盖每一种资源构造失败，见第 7 节 |
| P3 / R03 | `Dib32Layout` 集中尺寸与长度检查；DIB 存储一次初始化，GdiFlush 成功后才开放短期像素视图；采样与磨砂缓存迁移；冻结临时选择必先恢复 | 无效布局、旧像素拒绝、捕获/同步/分配失败恢复、缓存重试及 DC 复用测试均已执行 |
| P4 / R04 | 具名 unsafe 消息解码器集中解释 CREATESTRUCTW、DRAWITEMSTRUCT、NMCUSTOMDRAW、DPI RECT；绘图接收类型化引用；DPI 复制成值再排队 | 解码器类型与发送方验证 PASS；已修复默认 `WM_NCCREATE` 链遗漏；未知通知经过实际分发后调用默认过程的动态断言尚缺 |
| P5 / R05 | 纯算法移至 `ui/pixel_effects.rs` 并禁止 unsafe；仅 `ui::windows` 保留平台 gate；保持整数舍入、BGR 与 X 字节规则 | Windows 上纯算法字节一致性和非法输入测试 PASS；非 Windows 构建 NOT TESTED |
| P6 / R06 | `ControlSignal` 唯一持有 `OwnedHandle`；线程通过 `JoinHandle::as_handle` 借用；controller 转发 `BorrowedHandle`；host/probe 仅在等待期间转换原始 HANDLE | 4 项事件/线程句柄测试 PASS；controller 保持 `forbid(unsafe_code)`，输入队列和配对协议不变 |
| P7 / R07 | Cargo 全目标启用 `unsafe_op_in_unsafe_fn` 与 `undocumented_unsafe_blocks` deny；补调用点契约；安全边界文档、统计脚本、资源回归与报告 | 最终完整标准验证 PASS；首次失败与修复后桌面结果分开保留；性能对比仍 BLOCKED |

持续维护的所有权与安全契约见 [unsafe-boundaries.md](unsafe-boundaries.md)。

### 2.1 关键边界

根窗口的 `WM_NCDESTROY` 完成默认处理和 USERDATA 清理后才记录 `Destroyed`。
content 的结束不能替代 root 的结束；subclass 先移除自身，再继续默认处理链。
已记录根结束时 Drop 不再对旧 HWND 清 USERDATA 或重复销毁。
若 `DestroyWindow` 失败且不能证明根已结束，或返回成功却没有根结束证据，释放依赖字段前输出最小诊断并 abort。
这是极端生命周期失败分支的行为变化；普通创建、捕获、剪贴板忙和装饰失败仍沿用返回错误或降级。

私有 bitmap/DC 的恢复失败会使表面失效，先尝试删除私有 DC 解除选择；无法确认解除时保守保留关联原生资源并记录失败。
借来的 paint/control DC 不执行 DeleteDC。显式恢复返回结果，Drop 仅负责未完成时的兜底，不重复恢复。
字体更新仍先建立新资源，再更新全部相关控件，最后释放旧资源。
自绘按钮的 RestoreDC 失败会记录诊断并设置 Theme 的持续标志；旧字体换代及根销毁后字段析构前检查该标志，
保留可能仍被借用 DC 选中的字体，不向借用 DC 强行 DeleteDC。故障分支允许原生字体泄漏，正常路径不增加分配或引用计数。

DIB 原始切片只能在拥有存储的独占借用内短期使用，不允许逃逸或与 GDI 写入并行。
采样失败清除可读状态，缓存 origin 仅在捕获与处理全部成功后提交。
实时采样仍复用 1×1 表面；冻结捕获复用同一个 DC；没有新增逐次采样 Vec、DC、bitmap、后台线程或轮询定时器。
这些是代码与复用测试能支持的结构结论，不等同于完整性能比较 PASS。

### 2.2 实现偏离与理由

- `BitmapDc` 作为组合 owner 唯一拥有选中位图与私有 DC；DIB 的 `PixelStorage` 只保存指针、layout、可读状态等元数据。
  这样不会让两个结构分别拥有同一 bitmap，也不会让安全接口拆开其释放顺序。
- DC、DIB、窗口与字体调用方之间存在编译依赖，相关改动合并为可构建的本地提交，不机械制造 P1—P7 的七个中间提交。
  END_SHA 指向代码、测试、安全边界和脚本的实现提交；随后只补充文档证据的提交不修改生产/测试代码，避免报告自引用 SHA。
  实现提交：`f5e4eccd81a80f8c8a4896f22e1bd6515361fd2d`，`refactor: enforce native lifetime and resource boundaries`；随后文档提交主题为 `docs: record native unsafe refactoring evidence`。
- 最窄故障 seam 和隐藏窗口控制都只在测试构建中存在；没有项目级 mock 框架或 release 故障开关。
- 在原探针被用户已有单实例阻塞时，补充了独立进程的四窗口/表面资源回归。
  它覆盖销毁后的数量趋势；没有将该测试改称原探针的 baseline/final 性能对比。
- 为满足全目标 lint，部分非核心 Win32 文件、示例和既有测试仅增加 SAFETY 说明；没有借此扩展功能或改写应用架构。

## 3. 命令与真实执行结果

以下 PASS 都指对应日志中的那次执行；首次完整验证早于后续标题与字体恢复问题的最终修复，不能自动覆盖之后的代码。
日志保留于本地 `logs/`，不改写历史记录。命令在仓库根目录执行，未硬编码 Cargo 输出目录替代原脚本解析。

| 状态 | 命令 / 检查 | 退出码与证据 |
|---|---|---|
| PASS | `& .\scripts\verify-windows.ps1`，最终源码完整复测 | `0`；[retest-verify.log](../logs/unsafe-refactoring/retest-verify.log)、[退出码](../logs/unsafe-refactoring/retest-verify.exit.txt)；100 项库测试 + 68 项集成测试 = 168 项默认测试通过，23 项 ignored；fmt、全目标严格 Clippy、Release、实际嵌入 manifest、应用及 pixel-fixture PMv2 全部完成 |
| PASS | `& .\scripts\verify-windows.ps1`，首次整合后完整验证 | `0`；[final-verify.log](../logs/unsafe-refactoring/final-verify.log)、[退出码](../logs/unsafe-refactoring/final-verify.exit.txt)；97 项库测试 + 68 项集成测试 = 165 项默认测试通过，12 + 11 = 23 项 ignored；不重复累计 fatal 测试启动的子进程测试。含 fmt、Clippy、Release、实际嵌入 manifest、应用与 pixel-fixture 的 PerMonitorV2 检查 |
| PASS | `cargo clippy --all-targets --locked -- -D warnings`，该轮独立检查 | `0`；[final-clippy.log](../logs/unsafe-refactoring/final-clippy.log)、[退出码](../logs/unsafe-refactoring/final-clippy.exit.txt) |
| PASS | `cargo test --locked --lib platform::windows::gdi::tests -- --test-threads=1 --nocapture` | `0`，4 项；[gdi-tests.log](../logs/unsafe-refactoring/gdi-tests.log)、[退出码](../logs/unsafe-refactoring/gdi-tests.exit.txt) |
| PASS | `cargo test --locked --lib platform::windows::input::handle_tests -- --test-threads=1 --nocapture` | `0`，4 项；[R06 日志](../logs/measurements/unsafe-refactor-20260927/r06-focused-test-final.log)、[退出码](../logs/measurements/unsafe-refactor-20260927/r06-focused-test-final.exit.txt) |
| PASS | `cargo test --locked --lib native_settings_partial_creation_failures_end_every_root -- --test-threads=1 --nocapture` | `0`，1 项；[设置构造失败日志](../logs/measurements/unsafe-refactor-20260927/native-settings-partial.log)、[退出码](../logs/measurements/unsafe-refactor-20260927/native-settings-partial.exit.txt)；本机桌面执行 |
| PASS | `cargo test --locked --lib ui::windows::resource_regression::four_hidden_windows_and_surfaces_plateau_after_warmup -- --ignored --exact --test-threads=1 --nocapture` | `0`，1 项；[资源日志](../logs/unsafe-refactoring/retest-resource-regression.log)、[退出码](../logs/unsafe-refactoring/retest-resource-regression.exit.txt)；仅该隔离资源场景 |
| FAIL | 首轮逐项运行 17 项已筛选桌面回归 | 16 项 `0`，英文结果窗标题项 `101`；[命令/退出码汇总](../logs/unsafe-refactoring/desktop-results.json)，逐项见下节 |
| FAIL（已修正） | 增补 DIB panic 测试后的完整复测首尝试 | `1`；新增测试中 3 处宏内 unsafe 的 SAFETY 注释放在断言外，Clippy 拒绝；[原日志](../logs/unsafe-refactoring/retest-verify-lint-failure.log)保留。注释已调整，最终完整复测 PASS |
| FAIL（已修正） | 最终复测的 DIB 展开测试初版 | `retest-verify-panic-oracle-failure.log`，脚本退出 `1`、Cargo `101`；GetObjectType 对删除后句柄的判据不可靠，改为核对唯一 owner 的真实 DeleteObject 成功事件；最终完整复测 PASS |

R06 异常线程测试故意让无 hooks 的测试线程 panic，再验证借用结束后能 join 并得到错误结果。
日志中的该 panic 是覆盖的异常分支，4 项测试总退出码仍为 0。
R06 较早局部运行曾有未使用测试 helper 警告；上述后续全目标 Clippy 的实际成功记录才是 lint 验收证据。

### 3.1 新增与迁移覆盖

本表的 PASS 由最终完整 [retest-verify.log](../logs/unsafe-refactoring/retest-verify.log) 中具名测试支持，
不是根据源码中存在测试函数推断通过。

| 状态 | 必测目标 | 已执行的测试证据 |
|---|---|---|
| PASS | root/child/subclass 先结束，callback/字体后释放 | `real_native_tree_ends_before_callback_allocation_drops` |
| PASS | 原生先销毁 root，Rust Drop 不重复使用旧句柄 | `real_native_owner_destruction_is_not_repeated_by_rust_drop` |
| PASS | content 独立终止不误判 root | `native_result_content_and_external_root_destruction_are_distinct` |
| PASS | root/viewport/第 N 个控件创建失败 | `native_result_partial_creation_failures_end_every_root`、`native_settings_partial_creation_failures_end_every_root`、`native_create_rejection_after_attachment_is_terminal_before_free` |
| PASS | 无根结束证据时 abort，受保护字段不释放 | `failed_destroy_aborts_before_protected_fields_drop`；专用子进程 ready 存在、freed 不存在，并有正常析构对照 |
| PASS | BeginPaint 同步重入与 DPI 消息不冲突 | `begin_paint_reentry_can_deliver_dpi_without_conflicting_state_borrow`；真实原生消息重入 |
| PASS | GDI 构造/选择/恢复/双重失败 | `constructors_and_selection_fail_without_live_owned_objects`、两个 `failed_*restore*` bitmap 测试及 `failed_saved_dc_restore_is_reported_and_not_retried_by_drop` |
| PASS | 自绘字体恢复失败后不尝试释放旧/最终字体 | Result 与 Settings 的 `custom_draw_restore_failure_retains_replaced_and_final_*_fonts`；真实 WM_NOTIFY、RestoreDc 故障注入，正常各一次删除调用、故障零次，测试恢复 DC 后清理保留资源 |
| PASS | 临时 DIB 选择闭包展开 | `temporary_selection_unwind_restores_or_invalidates_before_bitmap_release`；catch_unwind 前验证真实选择且已到注入点，恢复成功可继续写读，恢复失败表面失效，临时 bitmap 成功释放一次 |
| PASS | DIB 无效布局、初始化、旧数据、恢复顺序 | `layout_rejects_invalid_dimensions_before_native_allocation`、`storage_is_initialized_and_failures_do_not_expose_old_pixels`、`temporary_selection_restores_after_capture_flush_allocation_errors_and_success`、`failed_temporary_restore_invalidates_surface_and_rejects_pixel_access` |
| PASS | 1×1 采样与冻结 DC 复用 | `sampling_reuses_one_pixel_and_freeze_reuses_its_dc` |
| PASS | 纯算法逐字节一致与边界输入 | `migrated_effect_matches_reference_byte_for_byte_including_x`、`invalid_input_does_not_modify_either_slice`、迁移的磨砂小图测试；包含单行、单列、不同 radius 和合法透明度 |
| PASS | 同 origin 缓存、变更与失败重试 | `cache_reuses_same_origin_and_retries_after_failed_refresh`、`ui_transparency_limit_and_invalid_layout_are_preserved` |
| PASS | 原生载荷一次解码、验证与值复制 | `typed_payloads_validate_sender_code_and_copy_dpi_rect`；使用初始化载荷和自有控件，不以随机地址充当可解引用指针 |
| PASS | 事件创建失败、跨线程 signal、正常/异常 join | `platform::windows::input::handle_tests` 4 项；另外保留移动合并、按下/释放配对等已有协议测试 |

### 3.2 逐项桌面回归

先列出 ignored 测试，再按实际存在的过滤器单独执行；未运行全部 ignored。
完整命令由 [retest-desktop-results.json](../logs/unsafe-refactoring/retest-desktop-results.json) 保存，执行清单见
[retest-desktop-regressions.ps1](../logs/unsafe-refactoring/retest-desktop-regressions.ps1)。
每项使用 `--ignored --exact --test-threads=1 --nocapture`；结果窗项以 `--lib` 和完整模块路径执行，其余以表中的 `--test` 目标执行。

| 目标 | 过滤器 | 最终状态 / 退出码 / 日志 |
|---|---|---|
| windows_capture | `screen_sampler_matches_known_pixels_and_releases_gdi_objects` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-windows_capture-screen_sampler_matches_known_pixels_and_releases_gdi_objects.log) |
| windows_preview | `preview_nonactivating_updates_and_resource_lifecycle` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-windows_preview-preview_nonactivating_updates_and_resource_lifecycle.log) |
| windows_preview | `hiding_an_intersecting_preview_restores_exact_underlying_sampling` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-windows_preview-hiding_an_intersecting_preview_restores_exact_underlying_sampling.log) |
| windows_selection_ui | `cached_selection_and_native_result_controls_smoke` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-windows_selection_ui-cached_selection_and_native_result_controls_smoke.log) |
| windows_selection_ui | `edge_freeze_preserves_initial_source_and_standard_window_size` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-windows_selection_ui-edge_freeze_preserves_initial_source_and_standard_window_size.log) |
| windows_selection_ui | `failed_capture_destroys_the_hidden_magnifier` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-windows_selection_ui-failed_capture_destroys_the_hidden_magnifier.log) |
| windows_settings_refresh | `unchanged_language_refresh_preserves_fonts_and_leaves_content_untouched` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-windows_settings_refresh-unchanged_language_refresh_preserves_fonts_and_leaves_content_untouched.log) |
| windows_settings_layout | `settings_header_and_content_recover_after_scrollbar_reflow` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-windows_settings_layout-settings_header_and_content_recover_after_scrollbar_reflow.log) |
| windows_language_dropdown | `language_choices_are_visible_on_the_first_open_after_applying` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-windows_language_dropdown-language_choices_are_visible_on_the_first_open_after_applying.log) |
| lib / result | `small_result_viewport_keeps_actions_visible_and_reveals_focused_rows` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-result-small_result_viewport_keeps_actions_visible_and_reveals_focused_rows.log) |
| lib / result | `feedback_reveals_footer_and_keeps_it_inside_the_work_area` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-result-feedback_reveals_footer_and_keeps_it_inside_the_work_area.log) |
| lib / result | `failed_host_copy_opens_manual_result_and_later_success_keeps_it_open` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-result-failed_host_copy_opens_manual_result_and_later_success_keeps_it_open.log) |
| lib / result | `quick_copy_success_keeps_focus_and_queues_cleanup_without_showing` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-result-quick_copy_success_keeps_focus_and_queues_cleanup_without_showing.log) |
| lib / result | `quick_copy_retries_hidden_then_reveals_failure_for_manual_recovery` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-result-quick_copy_retries_hidden_then_reveals_failure_for_manual_recovery.log) |
| lib / result | `quick_copy_non_busy_failure_reveals_result_immediately` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-result-quick_copy_non_busy_failure_reveals_result_immediately.log) |
| lib / result | `copy_feedback_preserves_layout_and_rejects_old_reset_timers` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-result-copy_feedback_preserves_layout_and_rejects_old_reset_timers.log) |
| lib / result | `english_result_labels_and_copy_feedback_fit_without_resizing` | PASS / 0 / [日志](../logs/unsafe-refactoring/retest-desktop-result-english_result_labels_and_copy_feedback_fit_without_resizing.log) |

首次失败实际断言为窗口标题空字符串，而预期是 `Picked color — Color Picker`。
审查定位到 result/settings 在 `WM_NCCREATE` 安装 userdata 后直接返回成功，遗漏原先的 `DefWindowProcW` 初始化链。
已在成功安装后继续默认过程，并保留 root tracker；最终 17 项均 PASS / 0。
首次 FAIL / 101 的[原始日志](../logs/unsafe-refactoring/desktop-result-english_result_labels_and_copy_feedback_fit_without_resizing.log)保留，未被成功复测覆盖。

上述用例只操作自己的窗口、合成图像或受控 fixture，不发送全局键鼠输入、不写用户剪贴板。
复制成功/失败场景验证注入结果、反馈和生命周期，不能代替真实剪贴板端到端验收。

另执行 `cargo test --locked --lib platform::windows::host::tests::blocked_pick_restores_settings_and_explains_without_applying -- --ignored --exact --test-threads=1 --nocapture`，
PASS / 0；[日志](../logs/unsafe-refactoring/retest-host-settings.log)。它只恢复自己的设置窗口，不应用配置。
加上隔离资源项，本轮单独执行的 ignored 测试共 19 项，全部通过；默认测试仍列 23 ignored，二者不混淆。

## 4. unsafe 与边界统计

工具为 [`scripts/unsafe-audit.py`](../scripts/unsafe-audit.py) 1.0，实际运行时 Python 3.12.14。
复现命令：

```powershell
python .\scripts\unsafe-audit.py --baseline 1a302eecd4d9954205cabff39186955e1bd6dcd3 --output logs/unsafe-refactoring/unsafe-audit-final.json
```

数据来自最终源码的 [unsafe-audit-final.json](../logs/unsafe-refactoring/unsafe-audit-final.json)，脚本实际退出 `0`。
摘要保存源码的 LF 归一化 SHA256，可与 END_SHA 的 Rust 源码核对；不因本机 CRLF 设置改变统计。

这不是 Rust AST 统计。词法命中按 `\bunsafe\b` 计算，含注释/字符串；另有屏蔽注释和字面量后的 code-token 数。
工具只识别明确的 `#[cfg(test)]` / `#[test]` 及测试外置模块，不展开宏、求值所有 cfg 或进行类型解析。
因此不把关键字次数称为精确 unsafe 块数或安全性评分。

| 范围 | 基线词法命中行 / 次数 | 当前词法命中行 / 次数 | 基线 → 当前 code-token |
|---|---:|---:|---:|
| 生产 `src`，排除识别到的测试 | 476 / 477 | 484 / 485 | 477 → 485 |
| `src` 内测试与测试外置模块 | 63 / 63 | 143 / 143 | 63 → 143 |
| 集成测试 `tests` | 246 / 246 | 250 / 250 | 246 → 250 |
| 示例 `examples` | 53 / 53 | 54 / 54 | 53 → 54 |
| build script | 0 / 0 | 0 / 0 | 0 → 0 |

生产关键字略增，来自拆小原有大块、unsafe helper 显式调用边界等；测试增加主要来自真实原生生命周期、故障与消息 fixture。
没有为降低数字合并大块或撤回检查。

| 独立边界指标 | 基线 → 当前 | 口径与解释 |
|---|---|---|
| DC/bitmap/font 类资源 owner Drop | 12 → 5 | 按脚本显式类型角色计数；不含 brush/pen、paint、选择 guard、窗口或内核 owner；当前加入独立 WindowDc 角色 |
| 同角色首个之外的重复 owner Drop | 7 → 0 | desktop DC、memory DC、bitmap、font 的释放责任收敛；不是所有 `impl Drop` 的总数 |
| UI 原生消息裸解码位置 | 12 → 5 | `lparam/payload.0` 转 CREATESTRUCTW、RECT、DRAWITEMSTRUCT、NMHDR、NMCUSTOMDRAW；当前集中在 4 个具名 decoder，通知头/完整结构分两步；不计二级 WindowInit、标量 HWND/HDC 或 userdata 取回 |
| 低级输入钩子载荷位置 | 2 → 2 | 同步钩子边界继续独立保留，不与 UI 消息解码混为一类 |
| 生产 DIB 原始切片构造 | 2 → 2 | 从 capture/frost 分散位置收敛到 `dib.rs` 的只读/可写两个短期入口；数量未下降，布局、初始化、同步与存活证明集中 |
| 窗口生命周期策略 | 4 个 owner 的分散清理 → 1 个共同状态机、4 个根绑定 | 每个根仍需证明整棵 child/subclass 树的结束，不能把复用 helper 误说成没有独立原生绑定 |
| 原始内核句柄接管 | 手写 HANDLE owner/Drop → 1 个 OwnedHandle 接管点 | BorrowedHandle 不转移关闭责任；等待结束后才 join |

## 5. 专项 diff 审查与已发现问题

独立审查覆盖 changed Drop、新增 unsafe、USERDATA/subclass 引用、原始切片、`mem::forget`、原始资源接管与 cfg 调整。
没有引入 `unsafe impl Send/Sync`、`transmute`、`Box::from_raw` 或通过 `'static` 绕过生命周期。
稳定 callback Box 由 owner 唯一释放；消息 helper 没有任意整数地址的公共 safe 解码器；未发现跨 DestroyWindow 的 RefMut。

审查推动了临时 bitmap 恢复 API 的 unsafe 来源契约、恢复/删除双重失败测试，以及临时 pen 先声明、SavedDc 后声明的释放顺序修正。
真实回归发现的 `WM_NCCREATE` 默认过程遗漏见第 3.2 节。
另发现按钮 custom draw 在 RestoreDC 失败后对控件字体的保守保留不足；已补 Theme 持续标志与字体换代/析构保护，
两项故障注入测试在最终完整验证中 PASS。普通字体及 DIB 删除测试曾使用不可靠的删除后句柄查询，
已改为记录真实 DeleteObject 调用（bitmap 还验证成功返回），不以 GDI 缓存/句柄复用推断析构是否执行。
上述静态审查不单独标记为动态 PASS，也不替代完整脚本和真实桌面回归。

## 6. 资源与性能

### 6.1 独立进程资源回归：PASS（限定场景）

最终实际输出见 [retest-resource-regression.log](../logs/unsafe-refactoring/retest-resource-regression.log)，
[退出码](../logs/unsafe-refactoring/retest-resource-regression.exit.txt)为 `0`。
实际执行及复现命令如下：

```powershell
cargo test --locked --lib ui::windows::resource_regression::four_hidden_windows_and_surfaces_plateau_after_warmup -- --ignored --exact --test-threads=1 --nocapture
```

同一测试进程创建/销毁预览、放大镜、结果、设置四种隐藏窗口，以及 sampler 和 DIB 表面；
不安装输入钩子、不生成系统输入、不触碰剪贴板。10 次预热后再执行两组各 25 次。

| 检查点 | 进程句柄 | GDI | USER |
|---|---:|---:|---:|
| 预热 10 次后 | 207 | 15 | 4 |
| 第 1 组 25 次后 | 207 | 15 | 4 |
| 第 2 组 25 次后，累计 50 次 | 207 | 15 | 4 |

最终运行耗时 5.05 秒，三个计数在两个检查组都未增加。
先前构建的同一场景为 209 / 14 / 4；它不是对照性能基线。两次测试进程的绝对缓存/句柄值不用于声称性能改善或退步。
这里包含测试宿主和缓存开销；该用例关闭透明背景采集并使用合成 Frozen 像素，不能代表真实 Live/Frozen 待机。
没有记录此场景的线程、工作集、CPU、唤醒或绘制延迟，因此这些指标不标 PASS。

### 6.2 baseline/final 性能对比：BLOCKED

复用项目原有 `resource-probe` 和被动测量脚本，汇总与原始文件 SHA256 见
[baseline-summary.json](../logs/measurements/unsafe-refactor-20260927/baseline-summary.json)。
测量开始时新增 GDI/lifetime 模块尚未接入运行路径，基线运行行为仍为 START_SHA；没有把完成版本冒充旧基线。

| 保存的基线二进制 | SHA256 |
|---|---|
| `baseline-binaries/color-picker.exe` | `C2ACF805C143EF59778A9B146C07229FA461CF63ECAB2D14C9481C4DFD44BFA6` |
| `baseline-binaries/resource-probe.exe` | `5B8DD0B384AD1D82D28935D353EB06F1E1C0D73941E8139708238F0EFF5043FC` |

二进制位于 `logs/measurements/unsafe-refactor-20260927/`，probe 构建命令为
`cargo build --release --locked --target x86_64-pc-windows-msvc --example resource-probe`。

| 命令 / 尝试 | 状态 / 退出码 | 实际阻塞与证据 |
|---|---|---|
| 保存的 `resource-probe.exe --cycles 100 --idle-seconds 10 --output <新文件>`，沙箱 | BLOCKED / 1 | 冷态空闲后 `0x80070005`，0 轮；[attempt-1](../logs/measurements/unsafe-refactor-20260927/baseline-probe-attempt-1.json) |
| 同一命令，本机桌面 | BLOCKED / 1 | 已有用户常驻实例占用单实例；[attempt-2](../logs/measurements/unsafe-refactor-20260927/baseline-probe-attempt-2.json) |
| `scripts/measure-passive-app.ps1 -ProcessId <已有PID> -Label baseline-existing-idle -ExpectedState 0 -Seconds 10 -OutputPath <新文件>`，沙箱 | BLOCKED / 1 | OpenQueryHandle 拒绝访问；[被动 attempt-1](../logs/measurements/unsafe-refactor-20260927/baseline-existing-passive-attempt.json) |
| 同一被动命令，本机桌面 | BLOCKED / 1 | 既有实例未开启 diagnostics；[被动 attempt-2](../logs/measurements/unsafe-refactor-20260927/baseline-existing-passive-attempt-2.json) |
| 最终 Release probe，同参数 `--cycles 100 --idle-seconds 10` | BLOCKED / 1 | 同样因已有实例拒绝开始，0 轮且 cleanup_completed=true；[最终 JSON](../logs/measurements/unsafe-refactor-20260927/final-probe-attempt.json)、[构建与执行日志](../logs/unsafe-refactoring/final-probe.log) |

首次 probe 的冷态空闲区间实际完成约 10.004 秒：句柄 129、GDI 0、USER 3、线程 4 前后一致，
工作集 10,694,656 → 10,698,752 bytes，私有提交 1,642,496 bytes。
该独立计时区间 GetProcessTimes 粒度内内核/用户 CPU 增量均为 0 ms；快照累计 CPU 包含快照自身开销。
这只是一段不完整 baseline 的冷态记录，不支持 Live/Frozen/结果/设置的性能或延迟结论。

最终 probe 以相同 MSVC Release 目标重新构建，构建退出 0；SHA256 为 `90C669BC6B22776FE4DC97B87AE74FC6FCDBBFD77FE7A6C3622F274ABF1902CA`。
用户实例未被结束，未为了测量开启其 diagnostics 或改变配置。
两组 Live 重复测量、Frozen/结果/设置静态资源/CPU，以及同机同构建配置的完成版本对比均缺失。
历史 `docs/measurements` 或 2026-09-20 的性能 PASS 只代表历史构建，不用于填补本次缺口。

## 7. 未完成验收与交付边界

| 状态 | 场景 | 原因 / 后续所需证据 |
|---|---|---|
| NOT TESTED | 第 13.2 节“每个资源构造阶段失败”的完整矩阵 | 现有故障注入覆盖 DesktopDc、bitmap、MemoryDc、SelectObject、SaveDC，但尚无 font/brush/pen、WindowDc 获取、BeginPaint 失败的对应执行断言；需验证部分构造时先前资源的清理与匹配结束调用 |
| NOT TESTED | DIB 原生分配后 NullStorage 失败的释放次数 | 当前测试验证返回错误，未独立断言该已分配 bitmap 恰好释放一次；需补原生释放事件证据 |
| NOT TESTED | SaveDC 失败的调用方回退与提前返回恢复顺序 | 当前测试验证保存失败返回 Err；尚缺调用方不改变 DC 状态，以及保存成功后提前返回时按序恢复的定向动态断言 |
| NOT TESTED | 第 13.2 节未知通知经过实际窗口分发后继续默认处理 | 现有测试直接调用 decoder 并验证 None；result/settings 生产 fallback 已存在，但缺少真实 dispatch/default-chain 的动态断言 |
| BLOCKED | baseline/final 空闲、Live、Frozen、结果、设置性能比较 | 已有用户实例与未开启 diagnostics；保留基线二进制，需在可安全独立测量的桌面补齐 |
| NOT TESTED | 非 Windows 的纯算法构建/运行 | 当前只有 Windows 原生执行；源码 gate 已拆分，但未执行非 Windows 工具链；没有启动 Docker |
| NOT TESTED | 干净 Windows 10、其他干净 Windows 11 环境 | 本轮仅当前 Windows 11；不能外推系统版本兼容性 |
| NOT TESTED | 混合 DPI 实际跨屏、负坐标副屏、显示器间隙、RDP/锁屏/休眠/热插拔 | 几何/映射测试覆盖部分输入；没有改变真实显示拓扑或会话环境 |
| NOT TESTED | 实际全局点击/滚轮吞吐、长按/双击与真实快捷键端到端 | 本轮不合成全局输入；协议测试不冒充真实输入验收 |
| NOT TESTED | 真实剪贴板竞争、自动复制、粘贴端到端 | 未覆盖用户剪贴板；注入复制结果与 native memory 解锁测试仅验证各自边界 |

已交付代码与测试、安全边界文档、可复现统计脚本、执行日志入口和本报告。
机器可读的[证据摘要](measurements/unsafe-refactoring-20260927-summary.json)随报告提交，包含完整命令、退出码、日志 SHA256、源码指纹与统计明细。
本机已执行的最终检查没有剩余 FAIL；历史失败仍保留记录。性能比较和上表环境项继续保留 BLOCKED / NOT TESTED。
未执行的 ignored 项还包括 onboarding guide、全局热键事务及两项 windows_shell；既有应用与未知热键占用状态使其不适合在本轮直接批量运行。

2026-09-27 二次逐项核对：以上新增测试缺口来自将计划条目与实际测试体逐项对照，
不是新执行失败或已确认的生产缺陷，也不能归为环境阻塞。特别需要补充 Surface 第二个字体、
Theme 第二个 brush 等部分构造失败后的清理，以及 SavedDc 调用方回退/提前返回断言。原有 168 / 19 项 PASS 仍对应其实际覆盖范围，
不能扩展为“第 13.2 节矩阵全部覆盖”。本次只修订核对记录，没有修改生产/测试代码或重跑桌面测试；
重新核对了实现提交源码指纹、24 份证据文件指纹和交付链接，均匹配。
