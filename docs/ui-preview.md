# 原生界面视觉更新

2026-09-21：结果与设置使用浅灰背景、白色卡片、圆角按钮与蓝色主操作；
颜色值使用等宽字体，中文使用 Microsoft YaHei UI。实时和冻结取色浮窗使用
统一的深色配色与分级文字，保留原有像素绘制和命中映射。

所有交互仍为原生 Win32 控件。只读色值可选择，Tab / Enter / Esc、复制重试、
设置验证和仅“应用”时保存的行为保留。行内复制按钮显示简短“复制”，
原生控件名称保留完整格式，供辅助技术区分。

主键现在点击后监听 A–Z、0–9 或 F1–F11，Esc / Tab / 离开控件可取消。
修饰键仍单独选择，按“应用”后才保存；监听只处理设置窗口的按键消息。
已消费的按键保持到释放，避免长按 Enter 或切换焦点时意外应用设置。

实时浮窗缩小为 208×58 DIP；冻结视口由 320 缩为 240 DIP，标准外框为
252×288 DIP，仅保留色块、HEX 和倍率。两种浮窗均移除操作说明，统一放在设置页。
冻结模式窗外左键取消取色，窗口内边框、留白和信息栏继续等待有效选择。

字体与画刷由窗口持有，字体只在 DPI / Surface 变化时重建；未增加常驻计时器、
输入钩子、动画或额外运行时依赖。本次未重做长时间性能测量。

## 本机检查

- Windows 11，窗口实际 DPI 为 168（175% 缩放）。下方图片为真实原生窗口截图。
- `scripts/verify-windows.ps1` 通过：格式、Clippy、77 项默认测试、x64 Release、
  嵌入 manifest 和 PerMonitorV2 检查。
- 已有 `windows_selection_ui` 桌面冒烟通过，覆盖冻结倍率映射、只读色值、
  按键录入 / F12 拒绝 / Esc / Tab / F10 系统键 / 多键长按与换焦点、
  设置验证、禁止保存与窗口销毁。窗外取消规则通过纯逻辑回归，未扩展为完整发布矩阵。
- 预览的置顶 / 不抢焦点 / 100 次资源释放检查通过：回到空闲后 GDI 为 0，USER 为 2，
  第 25 次与第 100 次均与预热基线一致。两种浮窗均在创建时声明置顶。

## 界面预览

独立预览不注册热键、不安装输入钩子、不读取屏幕像素、不写用户配置。
它使用合成颜色，通过生产窗口代码渲染，默认 60 秒后关闭：

```powershell
cargo run --release --example ui-preview -- result
cargo run --release --example ui-preview -- settings
cargo run --release --example ui-preview -- live
cargo run --release --example ui-preview -- frozen
```

可在模式后指定存活秒数（1–600）。预览中的“应用”只校验，不保存；
结果窗口的复制按钮在用户主动点击时仍会写入剪贴板。浮窗仅在这个合成预览中
允许截图，正常程序继续排除取色浮窗，避免采到自身画面。

### 取色结果

![取色结果](images/ui-result.png)

### 设置

![设置](images/ui-settings.png)

![监听主键](images/ui-settings-recording.png)

### 实时取色与冻结放大

![实时取色](images/ui-live.png)

![冻结放大](images/ui-frozen.png)
