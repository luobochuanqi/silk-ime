; Silk IME 安装与卸载（契约见 docs/design/foundation/07-installation.md）。
; package.ps1 注入版本与产物目录；文件事务和重启队列交由 Inno Setup 管理。
#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef BuildDir
  #define BuildDir "..\.tmp\build"
#endif

[Setup]
AppId={{9D4E5F60-7A8B-4C9D-8E0F-1A2B3C4D5E6F}
AppName=Silk 拼音
AppVersion={#AppVersion}
AppPublisher=Silk
AppSupportURL=https://github.com/luobochuanqi/silk-ime/issues
DefaultDirName={commonpf64}\Silk
DisableDirPage=yes
UsePreviousAppDir=no
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
MinVersion=10.0
; TSF 机器级注册和重启删除均需管理员权限，不再提供名义上的免提权模式。
PrivilegesRequired=admin
; TIP 会进入编辑器、浏览器等进程；即使静默升级，也不自动关闭用户应用。
CloseApplications=no
RestartApplications=no
WizardStyle=modern
OutputBaseFilename=Silk-Setup-{#AppVersion}
UninstallDisplayIcon={app}\silkhost.exe
SetupLogging=yes

[Languages]
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[Messages]
ConfirmUninstall=确定要卸载 %1 吗？%n%n请先切换到其他输入法，保存工作并关闭使用 Silk 的应用。仍被占用的程序文件将在重启后删除，不会强制结束应用。%n%n默认保留用户词库、配置和日志。
FinishedLabelNoIcons=Silk 拼音已安装。可用 Win+空格切换输入法。%n%n如提示重启，请重启后再使用，避免正在运行的应用继续加载旧版本。

[Files]
; 整包替换，避免 Go/Rust 产物版本资源不同导致协议两端混装。
; 不手动改名已加载的 DLL：重启替换、失败回滚和卸载清理由同一文件日志负责。
Source: "{#BuildDir}\silk-tsftip.dll"; DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete
Source: "{#BuildDir}\silkhost.exe"; DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete
Source: "{#BuildDir}\silkui.dll"; DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete
Source: "{#BuildDir}\dict\silk.dict"; DestDir: "{app}\dict"; Flags: ignoreversion restartreplace uninsrestartdelete

[Code]
const
  UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{9D4E5F60-7A8B-4C9D-8E0F-1A2B3C4D5E6F}_is1';
  InprocKey = 'Software\Classes\CLSID\{9D4E5F60-7A8B-4C9D-8E0F-1A2B3C4D5E6F}\InprocServer32';

var
  SetupError: String;
  LegacyNeedsRestart: Boolean;

function TipPath: String;
begin
  Result := ExpandConstant('{app}\silk-tsftip.dll');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  RegisteredDll: String;
begin
  Result := '';
  // 两个卸载器不能拥有同一个机器级 TIP；旧用户级安装必须先退出。
  if RegKeyExists(HKCU64, UninstallKey) or
     (RegQueryStringValue(HKLM64, InprocKey, '', RegisteredDll) and
      (CompareText(RegisteredDll, TipPath) <> 0)) then
    Result := '检测到其他目录或旧用户级安装。请先用原安装账户卸载旧版，' +
      '按提示重启，再安装此版本。卸载时可保留用户词库与配置。';
end;

function UpdateReadyMemo(Space, NewLine, MemoUserInfoInfo, MemoDirInfo,
  MemoTypeInfo, MemoComponentsInfo, MemoGroupInfo, MemoTasksInfo: String): String;
begin
  Result := '安装目录：' + ExpandConstant('{app}') + NewLine + NewLine +
    '为此计算机安装，并为发起安装的账户启用输入法。' + NewLine +
    '请保存工作、切换到其他输入法并关闭使用 Silk 的应用。' + NewLine +
    '仍被占用的文件将在重启后替换；安装器不会自动关闭应用。';
end;

function RunTIP(const Entry: String; OriginalUser: Boolean; var Error: String): Boolean;
var
  Started: Boolean;
  Code: Integer;
  Args: String;
begin
  Result := False;
  Error := '';
  if not FileExists(TipPath) then
  begin
    Error := '输入法组件缺失，请重新运行安装包修复后重试。';
    Log(Error);
    Exit;
  end;

  Args := '"' + TipPath + '",' + Entry;
  // 使用系统绝对路径，且等待专用 rundll32 入口的真实退出码。
  // ExecAsOriginalUser 只允许在安装期调用；不能在卸载器中借它恢复原用户。
  try
    if OriginalUser then
      Started := ExecAsOriginalUser(ExpandConstant('{sys}\rundll32.exe'), Args,
        ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code)
    else
      Started := Exec(ExpandConstant('{sys}\rundll32.exe'), Args,
        ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code);
    if not Started then
      Error := Format('%s 无法启动：%s（%d）。', [Entry, SysErrorMessage(Code), Code])
    else if Code <> 0 then
      Error := Format('%s 失败，退出码 %d。请查看执行账户的 Silk 日志。', [Entry, Code]);
  except
    Error := Entry + ' 执行失败：' + GetExceptionMessage;
  end;
  Result := Error = '';
  if Result then
    Log('TIP 操作完成：' + Entry)
  else
    Log(Error);
end;

procedure CleanLegacyDll;
var
  Path: String;
begin
  // 仅处理旧脚本产生的确切文件，不递归删除安装目录中的未知内容。
  Path := TipPath + '.old';
  if FileExists(Path) then
    if not DeleteFile(Path) then
    begin
      RestartReplace(Path, '');
      LegacyNeedsRestart := True;
      Log('旧版占用文件已登记为重启后删除：' + Path);
    end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then
  begin
    // NeedRestart 在文件事务结束时判定；旧文件必须在此之前登记，不能放到 ssPostInstall。
    CleanLegacyDll;
    Exit;
  end;
  if CurStep <> ssPostInstall then
    Exit;

  WizardForm.StatusLabel.Caption := '正在注册输入法…';
  if RunTIP('SilkRegister machine', False, SetupError) then
    RunTIP('SilkRegisterSelf', True, SetupError);

  // 注册失败时保留文件与卸载入口供修复；不撤销可能属于旧版的机器注册。
  // ssPostInstall 已越过文件事务，不能用一个异常假装完成了整体回滚。
  if SetupError <> '' then
  begin
    if not WizardSilent then
      SuppressibleMsgBox(SetupError + #13#10 +
        '安装未完成。请重新运行安装包修复；用户数据未改动。', mbError, MB_OK, IDOK);
  end;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if (CurPageID = wpFinished) and (SetupError <> '') then
  begin
    WizardForm.FinishedHeadingLabel.Caption := 'Silk 拼音安装未完成';
    WizardForm.FinishedLabel.Caption := SetupError + #13#10#13#10 +
      '程序文件已保留。请重新运行安装包修复，或从系统设置中卸载。';
  end;
end;

function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if SetupError <> '' then
    Result := 1;
end;

function NeedRestart: Boolean;
begin
  Result := LegacyNeedsRestart;
end;

function UninstallNeedRestart: Boolean;
begin
  Result := LegacyNeedsRestart;
end;

procedure RemoveUserData;
var
  Path: String;
begin
  // 静默卸载永远保留数据；提权可能切换账户，不能声称这是原安装用户的目录。
  if UninstallSilent then
    Exit;
  Path := ExpandConstant('{localappdata}\Silk');
  if not DirExists(Path) then
    Exit;

  // host 或旧 TIP 仍可能写回学习数据，不能一边运行一边清空词库。
  if FileExists(ExpandConstant('{app}\silkhost.exe')) or
     FileExists(TipPath) or FileExists(TipPath + '.old') then
  begin
    SuppressibleMsgBox('用户词库、配置和日志已保留。' + #13#10 +
      '部分程序仍在使用 Silk；如需清空数据，请重启后手动删除：' + #13#10 + Path,
      mbInformation, MB_OK, IDOK);
    Exit;
  end;

  if SuppressibleMsgBox('是否同时删除当前卸载账户的用户词库、配置和日志？' + #13#10 +
      Path + #13#10#13#10 + '此操作不可恢复；其他账户的数据不会删除。' + #13#10 +
      '选择“否”保留数据，便于以后重新安装。',
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES then
    if not DelTree(Path, True, True, True) then
    begin
      Log('用户数据未完全删除：' + Path);
      SuppressibleMsgBox('部分用户数据无法删除，已保留在：' + #13#10 + Path + #13#10 +
        '请关闭相关程序后手动清理。', mbInformation, MB_OK, IDOK);
    end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Error: String;
begin
  if CurUninstallStep = usUninstall then
  begin
    // 此事件位于用户确认之后、文件删除之前；注销失败即停止，保留修复所需的 DLL。
    if not RunTIP('SilkUnregister user', False, Error) then
      RaiseException(Error);
    if not RunTIP('SilkUnregister machine', False, Error) then
      RaiseException(Error);
    CleanLegacyDll;
  end
  else if CurUninstallStep = usPostUninstall then
    RemoveUserData;
end;
