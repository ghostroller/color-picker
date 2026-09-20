; Compile with Inno Setup 6.7.3 (Unicode); see installer/README.md.
; Package inputs are supplied by scripts/package-installer.ps1.
#ifndef AppVersion
  #error AppVersion must be supplied by the package script
#endif
#ifndef VersionInfoVersion
  #error VersionInfoVersion must be supplied as four numeric components
#endif
#ifndef PayloadDir
  #error PayloadDir must point to a complete, verified portable package
#endif
#ifndef OutputDir
  #error OutputDir must be supplied by the package script
#endif
#ifndef OutputBaseName
  #error OutputBaseName must be supplied by the package script
#endif

; Production identity MUST stay unchanged across releases. Test installers use
; a separate identity, Run value, shortcuts and directory without touching it.
#ifdef SmokeTestId
  #define ApplicationId "ColorPicker.Smoke." + SmokeTestId
  #define ApplicationName "Color Picker Smoke " + SmokeTestId
  #define StartupValueName "ColorPicker.Smoke." + SmokeTestId
#else
  #define ApplicationId "A5958C31-EDAE-4B6C-9A5F-CFC06081B191"
  #define ApplicationName "Color Picker"
  #define StartupValueName "Color Picker"
#endif
#ifndef DefaultInstallDir
  #define DefaultInstallDir "{localappdata}\Programs\" + ApplicationName
#endif
#define UninstallRegistryKey "Software\Microsoft\Windows\CurrentVersion\Uninstall\" + ApplicationId + "_is1"

[Setup]
AppId={#ApplicationId}
AppName={#ApplicationName}
AppVersion={#AppVersion}
VersionInfoVersion={#VersionInfoVersion}
AppPublisher=Color Picker
AppPublisherURL=https://github.com/ghostroller/color-picker
AppSupportURL=https://github.com/ghostroller/color-picker/issues
AppUpdatesURL=https://github.com/ghostroller/color-picker/releases
DefaultDirName={#DefaultInstallDir}
DefaultGroupName={#ApplicationName}
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
MinVersion=10.0.19045
UninstallDisplayIcon={app}\color-picker.exe
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBaseName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UsePreviousAppDir=yes
UsePreviousTasks=yes
UsePreviousLanguage=yes
CloseApplications=yes
CloseApplicationsFilter=color-picker.exe
RestartApplications=no
SetupLogging=yes
SetupMutex=Local\ColorPicker.Setup.{#ApplicationId}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimp"; MessagesFile: "languages\ChineseSimplified.isl"

[CustomMessages]
english.OptionalTasks=Preferences:
english.StartupTask=Start Color Picker when I sign in
english.DesktopTask=Create a desktop shortcut
english.LaunchApp=Start Color Picker
english.DowngradeBlocked=Color Picker %1 is already installed. This installer contains %2 and cannot install an older version. Uninstall the newer version first if you intentionally want to downgrade. Your saved preferences will be kept.
english.InstalledVersionUnknown=The installed Color Picker version could not be verified. Setup will not overwrite it. Repair or uninstall the existing installation first; saved preferences will be kept.
english.QuitFailed=Color Picker could not be closed safely (exit code %1). Exit it from its system tray menu, then retry. No application files have been changed.
english.InstallDirectoryChanged=Color Picker is already installed in %1. An upgrade must use the same directory. To move it, uninstall first and then install in the new location. Saved preferences will be kept.
english.StartupPathTooLong=The selected installation path makes the startup command longer than Windows' 260-character limit. Select a shorter path or clear the startup option.
chinesesimp.OptionalTasks=偏好设置：
chinesesimp.StartupTask=登录 Windows 时启动 Color Picker
chinesesimp.DesktopTask=创建桌面快捷方式
chinesesimp.LaunchApp=启动 Color Picker
chinesesimp.DowngradeBlocked=已安装 Color Picker %1，此安装包版本为 %2，不能覆盖较新版本。如确需降级，请先卸载已安装的版本；已保存的偏好设置会保留。
chinesesimp.InstalledVersionUnknown=无法验证已安装的 Color Picker 版本，安装程序不会覆盖它。请先修复或卸载现有安装；已保存的偏好设置会保留。
chinesesimp.QuitFailed=无法安全退出 Color Picker（退出代码 %1）。请从系统托盘菜单退出后重试。应用文件尚未更改。
chinesesimp.InstallDirectoryChanged=Color Picker 已安装在 %1，升级必须沿用此目录。如需迁移，请先卸载再安装到新目录；已保存的偏好设置会保留。
chinesesimp.StartupPathTooLong=所选安装目录导致启动项命令超过 Windows 的 260 字符限制。请选择较短的目录，或取消登录时启动选项。

[Tasks]
Name: "startup"; Description: "{cm:StartupTask}"; GroupDescription: "{cm:OptionalTasks}"; Flags: unchecked
Name: "desktopicon"; Description: "{cm:DesktopTask}"; GroupDescription: "{cm:OptionalTasks}"; Flags: unchecked

[Files]
; Never include user configuration in PayloadDir. All package licenses and
; documentation are retained, and only installed files enter the uninstall log.
Source: "{#PayloadDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "languages\INNO-SETUP-LICENSE.txt"; DestDir: "{app}\licenses\inno-setup"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#ApplicationName}"; Filename: "{app}\color-picker.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\{#ApplicationName}"; Filename: "{app}\color-picker.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[InstallDelete]
; An upgrade with this optional task deselected removes only our own shortcut.
Type: files; Name: "{autodesktop}\{#ApplicationName}.lnk"; Tasks: not desktopicon

[Registry]
; A quoted absolute target works with spaces and stays valid after an upgrade.
; Never change StartupApproved: Windows' disabled-startup choice remains its own.
Root: HKCU64; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#StartupValueName}"; ValueData: """{app}\color-picker.exe"" --startup"; Tasks: startup; Flags: uninsdeletevalue
Root: HKCU64; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "{#StartupValueName}"; Tasks: not startup; Flags: deletevalue

[Run]
Filename: "{app}\color-picker.exe"; Parameters: "--startup"; Description: "{cm:LaunchApp}"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent

[Code]
const
  InstalledKey = '{#UninstallRegistryKey}';

function CheckInstalledVersion: String;
var
  InstalledText: String;
  InstalledVersion, PackageVersion: Int64;
begin
  Result := '';
  if not RegKeyExists(HKCU64, InstalledKey) then
    Exit;

  // Setup owns and recreates this key after processing [Registry]. Use its
  // persisted DisplayVersion, never an earlier custom value that it can erase.
  if not RegQueryStringValue(HKCU64, InstalledKey, 'DisplayVersion', InstalledText) then
  begin
    Result := CustomMessage('InstalledVersionUnknown');
    Exit;
  end;

  if not StrToVersion(InstalledText, InstalledVersion) or
     not StrToVersion('{#VersionInfoVersion}', PackageVersion) then
  begin
    Result := CustomMessage('InstalledVersionUnknown');
    Exit;
  end;

  if ComparePackedVersion(InstalledVersion, PackageVersion) > 0 then
    Result := FmtMessage(CustomMessage('DowngradeBlocked'), [InstalledText, '{#AppVersion}']);
end;

function InitializeSetup: Boolean;
var
  Reason: String;
begin
  Reason := CheckInstalledVersion;
  Result := Reason = '';
  if not Result then
  begin
    Log(Reason);
    if not WizardSilent then
      MsgBox(Reason, mbError, MB_OK);
  end;
end;

function CloseInstalledApplication: String;
var
  Executable: String;
  ExitCode: Integer;
begin
  Result := '';
  Executable := ExpandConstant('{app}\color-picker.exe');
  if not FileExists(Executable) then
    Exit;

  // --quit only contacts an instance with this exact executable path. It waits
  // at most ten seconds, never starts a resident app, and never forcibly kills it.
  ExitCode := -1;
  if not Exec(Executable, '--quit', ExpandConstant('{app}'), SW_HIDE,
              ewWaitUntilTerminated, ExitCode) or (ExitCode <> 0) then
  begin
    Result := FmtMessage(CustomMessage('QuitFailed'), [IntToStr(ExitCode)]);
    Log(Result);
  end;
end;

function CheckInstallDestination: String;
var
  PreviousDirectory, SelectedDirectory: String;
begin
  Result := '';
  SelectedDirectory := ExpandConstant('{app}');
  if RegKeyExists(HKCU64, InstalledKey) then
  begin
    if not RegQueryStringValue(HKCU64, InstalledKey, 'InstallLocation', PreviousDirectory) or
       (Trim(PreviousDirectory) = '') then
    begin
      Result := CustomMessage('InstalledVersionUnknown');
      Exit;
    end;
    if CompareText(AddBackslash(ExpandFileName(PreviousDirectory)),
                   AddBackslash(ExpandFileName(SelectedDirectory))) <> 0 then
    begin
      Result := FmtMessage(CustomMessage('InstallDirectoryChanged'), [PreviousDirectory]);
      Exit;
    end;
  end;

  // Run/RunOnce stores a command line, whose documented maximum is 260 chars.
  if WizardIsTaskSelected('startup') and
     (Length(ExpandConstant('"{app}\color-picker.exe" --startup')) > 260) then
    Result := CustomMessage('StartupPathTooLong');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  // Recheck at the mutation boundary in case another installer ran meanwhile.
  Result := CheckInstalledVersion;
  if Result = '' then
    Result := CheckInstallDestination;
  if Result = '' then
    Result := CloseInstalledApplication;
end;

function InitializeUninstall: Boolean;
var
  Reason: String;
begin
  Reason := CloseInstalledApplication;
  Result := Reason = '';
  if not Result then
  begin
    Log(Reason);
    if not UninstallSilent then
      MsgBox(Reason, mbError, MB_OK);
  end;
end;
