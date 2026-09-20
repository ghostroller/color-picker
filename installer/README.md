# Windows installer source

`color-picker.iss` targets Inno Setup **6.7.3**, using its bundled English messages
and a vendored official Simplified Chinese translation. Simplified Chinese became
official after 6.7.3 was released and is **not** included in that compiler's install.
The pinned translation, original license and provenance are in `languages/`;
building the installer does not download them. The package script supplies these
required preprocessor definitions:

- `AppVersion`: the displayed project version, for example `0.1.0`.
- `VersionInfoVersion`: four numeric components, for example `0.1.0.0`.
- `PayloadDir`: absolute directory of the complete, verified portable package.
- `OutputDir` and `OutputBaseName`: the unique installer output directory/name.

The production AppId is the fixed GUID
`A5958C31-EDAE-4B6C-9A5F-CFC06081B191`. Keep it unchanged for upgrades. Installation
is per user, defaults to `%LOCALAPPDATA%\Programs\Color Picker`, requires native
x64 Windows 10 22H2 (build 19045) or later, and never requests elevation. This OS
gate does not replace the project's outstanding real-machine compatibility matrix.

The `startup` and `desktopicon` tasks are initially unchecked. Inno Setup preserves
their previous installer choices during upgrades; users can change either choice
by rerunning the installer. Startup uses the current user's `Run` value named
`Color Picker`, with the quoted installed executable and `--startup`. Windows may
separately disable startup through its own Startup Apps settings. A normal uninstall
removes this value, installed shortcuts, and files tracked by the installer. It
does not delete `%LOCALAPPDATA%\color-picker`, including `config.json` and logs.

Upgrades and uninstalls first run the installed executable with `--quit`, which
waits up to ten seconds for only an instance from the exact same executable path.
An error cancels the operation and asks the user to exit via the tray. Windows
Restart Manager remains enabled as a non-forcing fallback; automatic restart is
disabled. The final launch checkbox runs `--startup` only in interactive installs.

Downgrades are rejected before the wizard and checked again immediately before
installation. Comparison numerically parses Inno's `DisplayVersion` in the
product's HKCU 64-bit uninstall key. Packaging accepts numeric `major.minor.patch` only.
Unreadable version metadata also stops replacement. Silent installs fail without
showing the custom error dialog; `/LOG` captures the reason.

## Isolated smoke builds

Compile with `/DSmokeTestId=<unique-ASCII-id>` to derive a separate AppId, displayed
name, startup value, shortcuts and default install directory. Optionally provide
`/DDefaultInstallDir=<absolute-workspace-test-directory>` to keep installed files
in a dedicated workspace directory. Use the **same** test ID for an install,
upgrade and uninstall cycle, and a fresh one for a separate run. Do not launch the
test-installed app: its runtime configuration path intentionally remains the
production application's path. Silent setup already skips the post-install launch.

Useful setup switches: `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /LOG=<path>`;
`/TASKS="startup,desktopicon"` selects the two options and `/TASKS=""` clears them.
Omitting `/TASKS` on upgrade checks that prior choices are retained. Compile a newer
numeric version followed by an older one to verify that a silent downgrade returns
a nonzero exit code and leaves installed files/metadata unchanged.

## Official references

- [Compiler downloads and versions](https://jrsoftware.org/isdl.php)
- [Official translations](https://jrsoftware.org/files/istrans/)
- [Non-administrative install mode](https://jrsoftware.org/ishelp/topic_admininstallmode.htm)
- [Persistent AppId](https://jrsoftware.org/ishelp/topic_setup_appid.htm)
- [Preserving selected tasks](https://jrsoftware.org/ishelp/topic_setup_useprevioustasks.htm)
- [Registry value installation and removal](https://jrsoftware.org/ishelp/topic_registrysection.htm)
- [Restart Manager and CloseApplications](https://jrsoftware.org/ishelp/topic_setup_closeapplications.htm)
- [Numeric version parsing](https://jrsoftware.org/ishelp/topic_isxfunc_strtoversion.htm)
