// Included in [Code]. Only original files from published packages are eligible.
// No recursive deletion: user additions, modified files and reparse points stay.
const
  LegacyInvalidAttributes = $FFFFFFFF;
  LegacyDirectoryAttribute = $10;
  LegacyReparseAttribute = $400;

function LegacyGetFileAttributes(FileName: String): LongWord;
  external 'GetFileAttributesW@kernel32.dll stdcall';

function LegacyPlainDirectoryTree(Directory: String): Boolean;
var
  Attributes: LongWord;
  Parent: String;
begin
  Result := False;
  while Directory <> '' do
  begin
    Attributes := LegacyGetFileAttributes(Directory);
    if (Attributes = LegacyInvalidAttributes) or
       ((Attributes and LegacyDirectoryAttribute) = 0) or
       ((Attributes and LegacyReparseAttribute) <> 0) then
      Exit;
    Parent := ExtractFileDir(Directory);
    if CompareText(Parent, Directory) = 0 then
      Break;
    Directory := Parent;
  end;
  Result := True;
end;

function LegacyRelativePathAllowed(RelativePath: String): Boolean;
var
  I: Integer;
  Segment, LowerPath: String;
begin
  Result := False;
  if (RelativePath = '') or (RelativePath[1] = '/') or
     (RelativePath[Length(RelativePath)] = '/') then
    Exit;
  // Permit only forward-slash relative paths. Reject drive, UNC, ADS, wildcard
  // and Win32 normalization aliases even if a malformed manifest contains them.
  Segment := '';
  for I := 1 to Length(RelativePath) do
  begin
    if Pos(RelativePath[I], '\:*?"<>|') > 0 then
      Exit;
    if Ord(RelativePath[I]) < 32 then
      Exit;
    if RelativePath[I] = '/' then
    begin
      if (Segment = '') or (Segment = '.') or (Segment = '..') or
         (Segment[Length(Segment)] = '.') or (Segment[Length(Segment)] = ' ') then
        Exit;
      Segment := '';
    end
    else
      Segment := Segment + RelativePath[I];
  end;
  if (Segment = '') or (Segment = '.') or (Segment = '..') or
     (Segment[Length(Segment)] = '.') or (Segment[Length(Segment)] = ' ') then
    Exit;
  LowerPath := Lowercase(RelativePath);
  Result := (Copy(LowerPath, 1, 5) = 'docs/') or
    (Copy(LowerPath, 1, 9) = 'licenses/') or
    (LowerPath = 'cargo.lock') or (LowerPath = 'license-status.md') or
    (LowerPath = 'third-party-notices.md') or (LowerPath = 'build-info.json');
end;

function LegacyPlainFile(FileName: String): Boolean;
var
  Attributes: LongWord;
begin
  Attributes := LegacyGetFileAttributes(FileName);
  Result := (Attributes <> LegacyInvalidAttributes) and
    ((Attributes and (LegacyDirectoryAttribute or LegacyReparseAttribute)) = 0) and
    LegacyPlainDirectoryTree(ExtractFileDir(FileName));
end;

procedure LegacyRemoveEmptyParents(Directory, Root: String);
begin
  while (CompareText(Directory, Root) <> 0) and
        (CompareText(Copy(AddBackslash(Directory), 1, Length(AddBackslash(Root))),
                     AddBackslash(Root)) = 0) do
  begin
    if not LegacyPlainDirectoryTree(Directory) then
      Exit;
    // RemoveDir fails on nonempty directories; it never removes their contents.
    if not RemoveDir(Directory) then
      Exit;
    Directory := ExtractFileDir(Directory);
  end;
end;

procedure CleanupLegacyFiles;
var
  Lines: TArrayOfString;
  I, J: Integer;
  Root, Line, ExpectedHash, RelativePath, FileName: String;
  ValidHash: Boolean;
begin
  Root := ExpandFileName(ExpandConstant('{app}'));
  if not LegacyPlainDirectoryTree(Root) then
  begin
    Log('Legacy cleanup skipped: installation path contains a reparse point or is inaccessible.');
    Exit;
  end;
  try
    ExtractTemporaryFile('legacy-files.sha256');
    if not LoadStringsFromFile(ExpandConstant('{tmp}\legacy-files.sha256'), Lines) then
    begin
      Log('Legacy cleanup skipped: embedded manifest could not be read.');
      Exit;
    end;
    for I := 0 to GetArrayLength(Lines) - 1 do
    begin
      Line := Lines[I];
      if (Length(Line) > 66) and (Copy(Line, 65, 2) = '  ') then
      begin
        ExpectedHash := Copy(Line, 1, 64);
        ValidHash := True;
        for J := 1 to 64 do
          if Pos(Lowercase(ExpectedHash[J]), '0123456789abcdef') = 0 then
            ValidHash := False;
        RelativePath := Copy(Line, 67, Length(Line));
        if ValidHash and LegacyRelativePathAllowed(RelativePath) then
        begin
          StringChangeEx(RelativePath, '/', '\', True);
          FileName := AddBackslash(Root) + RelativePath;
          if LegacyPlainFile(FileName) then
          begin
            try
              if CompareText(GetSHA256OfFile(FileName), ExpectedHash) = 0 then
              begin
                // Recheck attributes immediately before the nonrecursive delete.
                if LegacyPlainFile(FileName) and DeleteFile(FileName) then
                begin
                  Log('Removed unchanged legacy release file: ' + RelativePath);
                  LegacyRemoveEmptyParents(ExtractFileDir(FileName), Root);
                end;
              end;
            except
              Log('Preserved inaccessible legacy file: ' + RelativePath);
            end;
          end;
        end;
      end;
    end;
  except
    // Cleanup is best-effort. Never turn a successful upgrade into a failure.
    Log('Legacy cleanup could not finish; remaining files were preserved.');
  end;
end;
