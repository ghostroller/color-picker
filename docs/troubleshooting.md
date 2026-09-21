# Troubleshooting

[English README](../README.md) · [中文说明](../README.zh-CN.md)

## The shortcut does nothing

1. Check the system tray, including hidden icons, to confirm the app is running.
2. Close Settings if it is open; picking is paused while that window is open.
3. Activate the tray icon or choose **开始取色** (Start picking) from its menu. This works even when the global shortcut could not be registered.
4. Open **设置** (Settings), choose an unused shortcut, and click **应用** (Apply). A shortcut needs Ctrl and/or Alt, optionally Shift, and one of A–Z, 0–9, or F1–F11. You can also click Apply to retry registering the existing shortcut.

Windows notification settings can suppress error notifications. If the issue persists, enable the optional file log below.

## Settings cannot be saved

The configuration file is `%LOCALAPPDATA%\color-picker\config.json`. This path is shared by installed and portable use.

An invalid, unreadable, or unsupported configuration is deliberately left untouched. The app uses defaults and disables saving. Exit through the tray, repair or move the file, and restart. A missing file is normal: defaults are loaded automatically and a file is created when you apply settings.

If another program changes the file while settings are being saved, the app cancels the save rather than overwriting the changed file. Restart and try again. If applying a new shortcut fails, the old shortcut and settings are retained.

## Copying fails

The result window reports clipboard errors. If another application is using the clipboard, wait for it to finish and click **复制** (Copy) again. The result's text remains selectable. A failed automatic copy does not require you to pick the color again.

## The sampled color differs from the source

The app samples the displayed desktop, not the original image file. Its scope is ordinary SDR, 8-bit RGB output. It does not promise HDR/WCG accuracy, original alpha, colors before ICC processing, or access to protected content and the UAC secure desktop. See [known limitations](known-limitations.md) for outstanding display and remote-session checks.

## Enable diagnostic logging

Logs are off by default. First exit any existing instance from its tray menu. From the source checkout, run:

```powershell
.\scripts\start-with-logs.ps1
```

The script builds into a separate `target/diagnostic` directory and starts a logging-enabled instance. It creates a unique file in `logs/` and prints its path and a live-view command. It does not close existing app instances.

**If an old instance is still running, the new process activates it and exits. This does not turn on logging in the old process.**

For an installed or portable executable, you can supply a log path directly. For example, run this from the folder containing the executable:

```powershell
.\color-picker.exe --log-file "$env:TEMP\color-picker-diagnostic.log"
```

`--log-file` also enables diagnostics and can be combined with `--check-environment`. `--diagnostics` alone enables on-demand host-state queries and does **not** write a log. Release builds have no console; read the log file directly.

| Log event | Meaning |
| --- | --- |
| `hotkey.registered` | The configured global shortcut was registered |
| `hotkey.registration_failed` | Registration failed; the following system error may indicate a conflict |
| `hotkey.received` | The host received `WM_HOTKEY` |
| `activation.handled` | Activation was handled and live picking started |
| `activation.ignored` | An existing preview caused a repeated activation to be ignored |
| `preview.started` | The sampling session and timer were created |
| `preview.stopped` | The timer and preview resources were released |
| `session.starting` / `session.finishing` | Waiting for input readiness / releasing a consumed gesture |
| `session.frozen` / `session.resumed_live` | Entered frozen zoom / resumed live picking |
| `result.shown` | The result window appeared after input and sampling cleanup |
| `config.applied` / `config.apply_failed` | Settings were saved / a failed change left previous settings active |
| `preview.sample_unavailable` / `preview.failed` | Sampling was temporarily unavailable / the preview stopped after an error |
| `tray.notification_accepted` | Windows accepted the notification request; this does not prove it was visible |
| `tray.balloon_show` | The shell reported showing the notification |
| `instance.existing` | Another instance was found; this log will not contain events from that process |

Logs contain discrete application events and errors, not general keystrokes, screen pixels, or clipboard contents. Logging adds no background thread or flush timer. Each log is limited to **1 MiB**; after `log.limit_reached`, use a new file path or rerun the script to start a new log.

## Developer checks

`color-picker.exe --check-environment` checks the effective DPI awareness and exits without installing input hooks. The main [developer guide](https://github.com/ghostroller/color-picker/blob/main/README.md#develop) covers build and test commands.

For a known pixel pattern, run `cargo run --locked --example pixel-fixture`. Its 384×256-pixel client area uses RGB `(x % 256, y % 256, (x ^ y) % 256)` at local `(x, y)`, except for the bottom 16 rows, which contain repeating single-pixel red, green, and blue columns. The title shows the client area's physical origin. Close the window when finished.

Use [UI previews](ui-preview.md) for isolated interface checks, [resource probes](resource-probe.md) for controlled start/cancel cycles, and [running-app measurements](performance-running-app.md) for measurements of an existing process. These tools are separate from the everyday app and do not add periodic measurements to it.
