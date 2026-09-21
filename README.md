<img src="resources/app.svg" width="64" height="64" alt="Color Picker icon">

# Color Picker

A native Windows color picker that stays in your system tray. Pick a pixel anywhere on your desktop, freeze and magnify a small area for precision, then copy its color into your design tool or code.

[Download for Windows](https://github.com/ghostroller/color-picker/releases) · [简体中文](README.zh-CN.md) · [UI previews](docs/ui-preview.md)

## Get started

1. Download the Windows x64 **installer** (`*-setup.exe`) or **portable ZIP** from [Releases](https://github.com/ghostroller/color-picker/releases).
2. Run the installer, or extract the entire ZIP and open `color-picker.exe`. No administrator privileges are required.
3. On your first manual launch, read the short guide, then find Color Picker in the system tray, including the hidden-icons area. Startup at Windows sign-in stays quiet.
4. Press **Ctrl + Alt + C**, move over the color you want, and **left-click** to select it.
5. In the result window, copy HEX, RGB, CSS RGB, or HSL. Choose **重新取色** (Pick again) to select another color.

The target platform is **Windows 11 x64**. Windows 10 22H2 x64 compatibility has not yet been fully verified. The app's interface is currently **Simplified Chinese**; the installer offers English and Simplified Chinese. Builds are currently unsigned. See [known limitations](docs/known-limitations.md) for the validation status.

## Pick precisely

The live preview follows your pointer and shows the color, HEX value, and physical screen coordinates. It also refreshes when the content under a stationary pointer changes.

| Action | Control |
| --- | --- |
| Start picking | **Ctrl + Alt + C**, activate the tray icon, or open the app again |
| Select the current live pixel | **Left-click** |
| Freeze the area and zoom in | **Scroll up**: 4× → 8× → 16× → 32× |
| Zoom out / return to live picking | **Scroll down**; scrolling below 4× resumes live picking |
| Select a frozen pixel | **Left-click inside the pixel grid** |
| Cancel picking | **Esc** or **right-click**; in frozen mode, left-clicking outside the preview also cancels |

The frozen preview uses a snapshot, so you can inspect a tiny target without chasing moving content. As you hover over its grid, the information bar updates the HEX value and the pixel's original screen X/Y coordinates alongside the zoom level. Clicking its border or information bar does not select a color. While picking, mouse clicks and scrolling are consumed by the picker; normal input resumes when the session ends.

![Frozen pixel grid with a color value, source coordinates, and zoom level](docs/images/ui-frozen.png)

Starting again during an active pick does not create another session. The tray menu shows your current shortcut and whether Windows registered it successfully. Closing a result window leaves the app in the tray. To quit completely, right-click the tray icon and choose **退出** (Exit).

## Copy the format you need

Every result includes the color swatch, original coordinates, and whether it came from live or frozen picking. Values are selectable, and each row has its own **复制** (Copy) button.

| Format | Example |
| --- | --- |
| HEX | `#FF0000` |
| RGB | `255, 0, 0` |
| CSS RGB | `rgb(255 0 0)` |
| HSL | `hsl(0 100% 50%)` |

The main Copy button uses your default format, initially HEX. A successful copy briefly changes that button to **已复制** (Copied), keeping the normal window layout stable. Only failures expand an error message. The result window supports **Tab**, **Enter**, and **Esc** for keyboard navigation and closing.

**Automatic copying and Quick pick are off by default.** Ordinary automatic copying still opens the result window. Enable **快速取色** (Quick pick) to always copy the default format and continue in your current app, without a result popup or focus change on success. If copying fails, the result window appears so you can retry. Your ordinary automatic-copy preference is retained while Quick pick is on and resumes when you turn it off.

![Color result with selectable values and copy controls](docs/images/ui-result.png)

## Make it yours

Right-click the tray icon and open **设置** (Settings).

| Setting | Options | Default |
| --- | --- | --- |
| Global shortcut | Ctrl and/or Alt, optional Shift, plus A–Z, 0–9, or F1–F11 | Ctrl + Alt + C |
| Default copy format | HEX, RGB, CSS RGB, HSL | HEX |
| Copy automatically after picking | On / off | Off |
| Quick pick: copy without opening a result | On / off; always copies the default format | Off |
| Border width | 0–6 DIP; 0 hides the border | 2 DIP |
| Picker background transparency | 0–80%; 0 is opaque | 35% |

To change the shortcut, select its modifiers, click the main-key button, and press the new letter, digit, or function key. **Esc** or moving focus away cancels key recording. F12 is reserved and cannot be used.

Settings fits the monitor's available height. On a small display or at high scaling, scroll the content; **应用** (Apply) stays visible at the bottom. The sample-color preview updates immediately as you drag the border and transparency sliders. It uses synthetic colors and saves nothing until you click Apply.

Click **应用** (Apply) to save, then close Settings before picking again. A pick request while Settings is open restores that window and explains why picking is paused, preserving your edits. Closing without applying discards your edits. If a new shortcut is unavailable or saving fails, the previous settings remain active. Picking previews and the result window share a right and bottom border. Transparency affects the picking previews' information backgrounds; color swatches and magnified pixels stay opaque.

Settings are stored in `%LOCALAPPDATA%\color-picker\config.json`, including for portable use. A missing file uses defaults. If a file is invalid, unreadable, or from an unsupported version, the app preserves it, uses defaults, and disables saving. Exit the app, repair or move that file, then restart; you do not need to create a replacement manually.

The welcome guide is remembered separately in `%LOCALAPPDATA%\color-picker\welcome-v1.seen`; showing it does not rewrite your settings.

## Install, update, and remove

- **Installer:** installs for the current user. Starting at Windows sign-in and creating a desktop shortcut are optional. Sign-in startup is off by default and can be disabled in Windows Startup apps.
- **Portable ZIP:** extract and run; it does not register a startup entry. Preferences still use the local app-data folder above.
- **Update:** download and run a newer installer. It preserves preferences and remembers installation choices. There is no background updater.
- **Uninstall:** remove Color Picker from Windows Installed apps. Your preferences are retained; move or delete `config.json` after exiting if you also want to reset them.

See [Windows installation and maintenance](docs/windows-installer.md) for paths, startup behavior, and upgrade details.

## Privacy and limitations

Color Picker makes no network requests and collects no telemetry. Captured screen data stays in memory. Picking resources and input hooks are released when a session ends.

Sampling is designed for ordinary **8-bit RGB SDR desktops**. HDR/WCG, original alpha, colors before ICC processing, protected content, and the UAC secure desktop are outside the supported scope. Mixed-DPI setups, display changes, RDP, and other edge cases still need broader testing. This version has no color history or saved palettes.

If the shortcut does nothing, check its registration status in the tray menu, try the tray icon, and close Settings if it is open. For shortcut conflicts, configuration recovery, clipboard failures, and optional file logging, see [Troubleshooting](docs/troubleshooting.md). [Known limitations](docs/known-limitations.md) and [validation records](docs/validation.md) describe what has and has not been checked.

## Develop

The app uses **Rust**, the `windows` crate, native **Win32 controls**, and **GDI**. No web runtime is required.

Build on Windows x64 with Rust (via rustup), Visual Studio C++ Build Tools, and the Windows SDK. The repository pins Rust in [`rust-toolchain.toml`](rust-toolchain.toml) and dependencies in `Cargo.lock`.

```powershell
cargo run --locked
.\scripts\verify-windows.ps1
```

The verification script checks formatting, Clippy, default tests, an x64 Release build, the embedded manifest, and effective PerMonitorV2 DPI awareness. The Release executable is normally written to `target/x86_64-pc-windows-msvc/release/color-picker.exe`.

Desktop tests require an interactive Windows session. Exit existing Color Picker instances first, keep test windows unobscured, and run them serially:

```powershell
cargo test --locked --lib -- --ignored --test-threads=1
cargo test --locked --test windows_capture --test windows_preview --test windows_shell --test windows_selection_ui -- --ignored --test-threads=1 --nocapture
```

These tests create temporary windows and app instances, exercise input-hook setup, and temporarily use the default shortcut. They do not generate real keyboard/mouse input or change the clipboard, and do not replace manual display and input testing. Non-Windows systems can run the platform-independent tests with `cargo test --lib --tests --locked`; the app itself is Windows-only.

For automation, pass `--no-onboarding` to suppress the welcome guide without marking it as seen; the next ordinary manual launch can still show it. `--startup` also suppresses the guide and quietly exits when another instance is already running. The optional `quick_pick` setting defaults to `false`, including when loading older configuration files that omit it.

| Location | Purpose |
| --- | --- |
| `src/core/` | Color formatting, geometry, zoom mapping, and session state |
| `src/app/` | Settings, command-line options, diagnostics, and coordination |
| `src/platform/windows/` | Capture, input, global shortcut, tray, clipboard, and process lifecycle |
| `src/ui/windows/` | Live and frozen previews, result window, and settings |
| `tests/`, `examples/` | Logic tests, desktop checks, UI previews, and resource probes |
| `scripts/`, `installer/` | Verification, packaging, and the per-user installer |

Build a portable package with `.\scripts\package-windows.ps1`. For an installer, install the pinned Inno Setup version described in [Windows installation](docs/windows-installer.md), then run `.\scripts\package-installer.ps1`. Outputs go to `dist/` with SHA256 checksums. CI verification and packaging workflows are **manually triggered**; pushes and tags do not start them automatically.

Further developer documentation (primarily Chinese): [UI previews](docs/ui-preview.md), [diagnostic logging](docs/troubleshooting.md), [resource probes](docs/resource-probe.md), [running-app measurements](docs/performance-running-app.md), [CI and releases](docs/ci-packaging.md), [implementation plan](docs/implementation-plan.md), and [development records](docs/progress.md). The application icon's vector source is [`resources/app.svg`](resources/app.svg); regenerate its Windows ICO with [the icon script](scripts/generate-icon.ps1).

## License

A project redistribution license has not yet been selected. See [LICENSE-STATUS.md](LICENSE-STATUS.md). Third-party dependencies retain their own licenses; packaged builds include their license and notice files.
