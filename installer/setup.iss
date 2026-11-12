; Silk IME 安装脚本（规格：docs/design/foundation/07-installation.md）
; 版本号与产物目录由 package.ps1 经 /DAppVersion、/DBuildDir 注入。
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
DefaultDirName={autopf}\Silk
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
CloseApplications=yes
OutputBaseFilename=Silk-Setup-{#AppVersion}
UninstallDisplayIcon={app}\silkhost.exe
SetupLogging=yes

[Files]
; restartreplace：改名占位也失败时延迟到重启后替换（07 升级策略 3）
Source: "{#BuildDir}\silk-tsftip.dll"; DestDir: "{app}"; Flags: restartreplace
Source: "{#BuildDir}\silkhost.exe"; DestDir: "{app}"
Source: "{#BuildDir}\silkui.dll"; DestDir: "{app}"
Source: "{#BuildDir}\dict\*"; DestDir: "{app}\dict"

[UninstallDelete]
Type: files; Name: "{app}\silk-tsftip.dll.old"

[Code]
const
  Rundll = 'rundll32.exe';

function DllArg(const Entry: String): String;
begin
  Result := '"' + ExpandConstant('{app}\silk-tsftip.dll') + '",' + Entry;
end;

// 07 升级策略 2：占用中的 TIP DLL 改名占位（Windows 允许 rename 打开中的文件），
// 新文件落原名，多数场景免重启。
// ponytail: 改名后安装中途失败不会自动改回，靠下次安装清理 .old；完整回滚待升级恢复机制。
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  DllPath: String;
begin
  DllPath := ExpandConstant('{app}\silk-tsftip.dll');
  if FileExists(DllPath) then
  begin
    if FileExists(DllPath + '.old') then
      DeleteFile(DllPath + '.old');
    if not RenameFile(DllPath, DllPath + '.old') then
      NeedsRestart := True;
  end;
  Result := '';
end;

// 注册链路（03 ADR-6）：机器级三件套必须提权（调研 §1.3：非提权一律 E_FAIL）；
// 用户级补登记必须以发起安装的原始用户执行，否则登记进管理员账户（07 踩坑项）。
procedure RegisterTIP;
var
  Code: Integer;
begin
  if IsAdminInstallMode then
    Exec(Rundll, DllArg('SilkRegister machine'), '', SW_HIDE, ewWaitUntilTerminated, Code)
  else
    ShellExec('runas', Rundll, DllArg('SilkRegister machine'), '', SW_HIDE, ewWaitUntilTerminated, Code);
  ExecAsOriginalUser(Rundll, DllArg('SilkRegisterSelf'), '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure UnregisterTIP;
var
  Code: Integer;
begin
  ExecAsOriginalUser(Rundll, DllArg('SilkUnregister user'), '', SW_HIDE, ewWaitUntilTerminated, Code);
  if IsAdminInstallMode then
    Exec(Rundll, DllArg('SilkUnregister machine'), '', SW_HIDE, ewWaitUntilTerminated, Code)
  else
    ShellExec('runas', Rundll, DllArg('SilkUnregister machine'), '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    RegisterTIP;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    UnregisterTIP
  else if CurUninstallStep = usPostUninstall then
  begin
    // 07 卸载：默认保留用户数据；/VERYSILENT（更新器路径）走默认值即保留。
    if SuppressibleMsgBox('是否同时删除用户词库与配置？' + #13#10 +
        '选择「否」保留 ' + ExpandConstant('{localappdata}\Silk') + '。', mbConfirmation, MB_YESNO, IDNO) = IDYES then
      DelTree(ExpandConstant('{localappdata}\Silk'), True, True, True);
  end;
end;
