// Included in [Code]. This check is independent of the installed executable,
// and remains available to Uninstall when that executable cannot be started.
const
  InstallationSnapshotProcesses = $2;
  InstallationQueryAndSynchronize = $00101000;
  InstallationWaitSignaled = 0;
  InstallationNoMoreFiles = 18;
  InstallationInvalidParameter = 87;
  InstallationOpenExisting = 3;
  InstallationShareAll = $7;
  InstallationBackupSemantics = $02000000;

type
  // PROCESSENTRY32W is 556 bytes in x86 and 568 bytes in x64. Use an explicit
  // 568-byte buffer rather than depend on Pascal Script record alignment.
  // The native dwSize limits writes; PID is at byte 8 in both layouts and
  // szExeFile starts at byte 36 (x86) or 44 (x64).
  TInstallationProcessEntry = array[0..283] of Word;

function InstallationSnapshot(Flags, ProcessId: LongWord): THandle;
  external 'CreateToolhelp32Snapshot@kernel32.dll stdcall';
function InstallationProcessFirst(Snapshot: THandle; var Entry: TInstallationProcessEntry): Boolean;
  external 'Process32FirstW@kernel32.dll stdcall';
function InstallationProcessNext(Snapshot: THandle; var Entry: TInstallationProcessEntry): Boolean;
  external 'Process32NextW@kernel32.dll stdcall';
function InstallationOpenProcess(Access: LongWord; Inherit: Boolean; ProcessId: LongWord): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function InstallationImagePath(Process: THandle; Flags: LongWord; Path: String; var Capacity: LongWord): Boolean;
  external 'QueryFullProcessImageNameW@kernel32.dll stdcall';
function InstallationWait(Process: THandle; Milliseconds: LongWord): LongWord;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function InstallationCloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';
function InstallationOpenPath(Path: String; Access, Sharing: LongWord; Security: THandle;
  Creation, Flags: LongWord; Template: THandle): THandle;
  external 'CreateFileW@kernel32.dll stdcall';
function InstallationFinalPath(Handle: THandle; Path: String; Capacity, Flags: LongWord): LongWord;
  external 'GetFinalPathNameByHandleW@kernel32.dll stdcall';

function InstallationCanonicalPath(Path: String; var Canonical: String): Boolean;
var
  Handle: THandle;
  Count: LongWord;
begin
  Result := False;
  Handle := InstallationOpenPath(Path, 0, InstallationShareAll, 0,
    InstallationOpenExisting, InstallationBackupSemantics, 0);
  if Handle = THandle(-1) then
  begin
    Log('Process check could not open executable path: ' + Path +
      '; error=' + IntToStr(DLLGetLastError));
    Exit;
  end;
  try
    Canonical := StringOfChar(#0, 32768);
    Count := InstallationFinalPath(Handle, Canonical, Length(Canonical), 0);
    if (Count = 0) or (Count >= LongWord(Length(Canonical))) then
    begin
      Log('Process check could not resolve executable path: ' + Path +
        '; error=' + IntToStr(DLLGetLastError));
      Exit;
    end;
    SetLength(Canonical, Count);
    Result := True;
  finally
    if not InstallationCloseHandle(Handle) then
    begin
      Log('Process check could not close its path handle.');
      Result := False;
    end;
  end;
end;

function InstallationEntryName(var Entry: TInstallationProcessEntry; NameOffset: Integer): String;
var
  I: Integer;
begin
  Result := '';
  // Both possible names of our executable (long name and its 8.3 alias) are
  // ASCII. A different Unicode basename cannot be either candidate.
  for I := 0 to 259 do
  begin
    if Entry[NameOffset + I] = 0 then
      Exit;
    if Entry[NameOffset + I] > 127 then
    begin
      Result := '';
      Exit;
    end;
    Result := Result + Chr(Entry[NameOffset + I]);
  end;
  Result := '';
end;

function InstallationCandidateIdle(ProcessId: LongWord; Target: String): Boolean;
var
  Process: THandle;
  Path, Canonical: String;
  Capacity: LongWord;
  QueryError: Integer;
begin
  Result := False;
  Process := InstallationOpenProcess(InstallationQueryAndSynchronize, False, ProcessId);
  if Process = 0 then
  begin
    QueryError := DLLGetLastError;
    // This specific error means the snapshot PID no longer names a process.
    // Access denied and every other unknown condition must stop replacement.
    Result := QueryError = InstallationInvalidParameter;
    if not Result then
      Log('Process check could not open candidate PID ' + IntToStr(ProcessId) +
        '; error=' + IntToStr(QueryError));
    Exit;
  end;
  try
    Path := StringOfChar(#0, 32768);
    Capacity := Length(Path);
    if not InstallationImagePath(Process, 0, Path, Capacity) then
    begin
      QueryError := DLLGetLastError;
      // Retaining a process handle prevents PID reuse from changing identity.
      // A terminated candidate is harmless even if its image query now fails.
      Result := InstallationWait(Process, 0) = InstallationWaitSignaled;
      if not Result then
        Log('Process check could not query candidate PID ' + IntToStr(ProcessId) +
          '; error=' + IntToStr(QueryError));
      Exit;
    end;
    SetLength(Path, Capacity);
    if not InstallationCanonicalPath(Path, Canonical) then
    begin
      Result := InstallationWait(Process, 0) = InstallationWaitSignaled;
      Exit;
    end;
    Result := CompareText(Canonical, Target) <> 0;
    if not Result then
    begin
      Result := InstallationWait(Process, 0) = InstallationWaitSignaled;
      if not Result then
        Log('Process check found a running instance at the installation path; PID=' + IntToStr(ProcessId));
    end;
  finally
    if not InstallationCloseHandle(Process) then
    begin
      Log('Process check could not close its candidate handle.');
      Result := False;
    end;
  end;
end;

function InstallationPathIdle(Executable: String): Boolean;
var
  Snapshot: THandle;
  Entry: TInstallationProcessEntry;
  Canonical, Name, ShortName: String;
  NameOffset: Integer;
  ProcessId: LongWord;
begin
  Result := False;
  if (SizeOf(Entry) <> 568) or
     ((SizeOf(Snapshot) <> 4) and (SizeOf(Snapshot) <> 8)) then
  begin
    Log('Process check has an unsupported native structure layout.');
    Exit;
  end;
  if not InstallationCanonicalPath(Executable, Canonical) then
    Exit;
  Name := ExtractFileName(Executable);
  ShortName := ExtractFileName(GetShortName(Executable));
  if SizeOf(Snapshot) = 8 then
  begin
    Entry[0] := 568;
    NameOffset := 22;
  end
  else
  begin
    Entry[0] := 556;
    NameOffset := 18;
  end;
  Entry[1] := 0;
  Snapshot := InstallationSnapshot(InstallationSnapshotProcesses, 0);
  if Snapshot = THandle(-1) then
  begin
    Log('Process check could not create a process snapshot; error=' + IntToStr(DLLGetLastError));
    Exit;
  end;
  try
    if not InstallationProcessFirst(Snapshot, Entry) then
    begin
      Result := DLLGetLastError = InstallationNoMoreFiles;
      Exit;
    end;
    repeat
      if (CompareText(InstallationEntryName(Entry, NameOffset), Name) = 0) or
         ((ShortName <> '') and
          (CompareText(InstallationEntryName(Entry, NameOffset), ShortName) = 0)) then
      begin
        ProcessId := LongWord(Entry[4]) or (LongWord(Entry[5]) shl 16);
        if not InstallationCandidateIdle(ProcessId, Canonical) then
          Exit;
      end;
    until not InstallationProcessNext(Snapshot, Entry);
    Result := DLLGetLastError = InstallationNoMoreFiles;
    if not Result then
      Log('Process check could not complete process enumeration; error=' + IntToStr(DLLGetLastError));
  finally
    if not InstallationCloseHandle(Snapshot) then
    begin
      Log('Process check could not close its snapshot handle.');
      Result := False;
    end;
  end;
end;
