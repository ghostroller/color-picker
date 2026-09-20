#ifndef ExpectedInnoVersion
  #error ExpectedInnoVersion is required
#endif
#if DecodeVer(Ver) != ExpectedInnoVersion
  #error Inno Setup version mismatch; install the pinned version in installer/toolchain.json
#endif

; Check the loaded compiler and preprocessor, without producing/installing files.
[Setup]
AppName=Color Picker compiler check
AppVersion=0
DefaultDirName={tmp}\ColorPickerCompilerCheck
Output=no
Uninstallable=no
CreateAppDir=no
