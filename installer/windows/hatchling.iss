; The Windows setup. It holds a release's files and installs for this user alone,
; with no administrator rights; setup.ps1 does the rest, in a console the owner
; can see. Nothing here asks for a key, a PIN or a code.
;
;   ISCC.exe /DReleaseDir=<dir> /DAppVersion=0.8.0 installer\windows\hatchling.iss
;
; <dir> holds what a release publishes: the programs, hatchling-ui.tar.gz,
; hatchling-installer.tar.gz and SHA256SUMS.
#define AppName "Hatchling"
#ifndef AppVersion
  #define AppVersion "0.8.0"
#endif
#ifndef ReleaseDir
  #error Pass /DReleaseDir=<dir>, a directory of release files.
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif
#ifndef Distro
  #define Distro "hatchling-wsl"
#endif
#define Script "powershell.exe"
; RemoteSigned, not Bypass: enough for scripts installed here, and with Bypass and the
; Startup entry together, Microsoft Defender calls this setup a trojan (Bearfoos.A!ml,
; a machine-learning guess). Scan each build before publishing it.
#define Run "-NoProfile -ExecutionPolicy RemoteSigned -File"

[Setup]
AppId={{7E0C6A52-4F0B-4B6E-9C3E-5A1D8E2B9F41}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Ocean Bennett
AppCopyright=Copyright (c) 2026 Ocean Bennett
AppPublisherURL=https://github.com/undergroundrap/hatchling
DefaultDirName={autopf}\Hatchling
DefaultGroupName=Hatchling
DisableProgramGroupPage=yes
DisableWelcomePage=no
SetupIconFile=..\..\frontend\public\favicon.ico
WizardSmallImageFile=wizard-small.bmp
WizardImageFile=wizard-large.bmp
UninstallDisplayIcon={app}\hatchling.ico
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=Hatchling-{#AppVersion}-Setup
Compression=lzma
SolidCompression=yes
WizardStyle=modern
UninstallDisplayName=Hatchling
InfoBeforeFile=notice.txt

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Messages]
WelcomeLabel1=Hatchling {#AppVersion}
WelcomeLabel2=Secure infrastructure for always-on AI agents you own.%n%nCreated by Ocean Bennett.%nCopyright (c) 2026 Ocean Bennett.

[Files]
Source: "common.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "setup.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "open.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "broker.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\frontend\public\favicon.ico"; DestDir: "{app}"; DestName: "hatchling.ico"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "{#ReleaseDir}\*"; DestDir: "{app}\release"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Hatchling"; Filename: "{#Script}"; Parameters: "{#Run} ""{app}\open.ps1"" -Distro {#Distro}"; IconFilename: "{app}\hatchling.ico"; Comment: "Open Hatchling"
Name: "{autoprograms}\Hatchling Tools\Protect your vault"; Filename: "{#Script}"; Parameters: "{#Run} ""{app}\broker.ps1"" -Distro {#Distro} -Do protect"; IconFilename: "{app}\hatchling.ico"
Name: "{autoprograms}\Hatchling Tools\Make a backup"; Filename: "{#Script}"; Parameters: "{#Run} ""{app}\broker.ps1"" -Distro {#Distro} -Do backup"; IconFilename: "{app}\hatchling.ico"
Name: "{autoprograms}\Hatchling Tools\Unlock"; Filename: "{#Script}"; Parameters: "{#Run} ""{app}\broker.ps1"" -Distro {#Distro} -Do unlock"; IconFilename: "{app}\hatchling.ico"
; WSL stops a distro soon after its last terminal closes: this keeps it running
; while the owner is logged in, with no window.
Name: "{userstartup}\Hatchling"; Filename: "{sys}\conhost.exe"; Parameters: "--headless wsl.exe -d {#Distro} --exec sleep infinity"; Tasks: keeprunning

[Tasks]
Name: "keeprunning"; Description: "Keep Hatchling running while I'm logged in (recommended)"
Name: "backupfirst"; Description: "Make a backup before updating (recommended). With a security key, it asks for your PIN and a touch"; Check: Updating

[Run]
Filename: "{#Script}"; Parameters: "{#Run} ""{app}\setup.ps1"" -Distro {#Distro} -Version {#AppVersion} {code:SetupArguments}"; StatusMsg: "Setting Hatchling up in WSL. Follow the console window."; Flags: waituntilterminated
Filename: "{#Script}"; Parameters: "{#Run} ""{app}\open.ps1"" -Distro {#Distro}"; Description: "Open Hatchling"; Flags: postinstall nowait skipifsilent

[UninstallRun]
Filename: "wsl.exe"; Parameters: "--terminate {#Distro}"; Flags: runhidden; RunOnceId: "StopDistro"

[Code]
var
  ModelPage: TInputOptionWizardPage;
  WasInstalled: Boolean;

// Asked once, before anything is copied: afterwards, the files are always there.
function InitializeSetup: Boolean;
begin
  WasInstalled := FileExists(ExpandConstant('{autopf}\Hatchling\setup.ps1'));
  Result := True;
end;

procedure InitializeWizard;
begin
  ModelPage := CreateInputOptionPage(wpSelectTasks, 'Choose a model',
    'What Hatchling thinks with.',
    'You can switch later, in the app. Either way, Hatchling runs on this PC.', True, False);
  ModelPage.Add('On this PC, with Ollama: private, and needs a good GPU and about 7 GB');
  ModelPage.Add('Hosted, with OpenRouter: no GPU needed, and your messages go to a provider. You''ll need a key');
  ModelPage.Values[0] := True;
end;

function Updating: Boolean;
begin
  Result := WasInstalled;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := Updating and (PageID = ModelPage.ID);
end;

function SetupArguments(Param: String): String;
begin
  if Updating then begin
    if WizardIsTaskSelected('backupfirst') then Result := '-Backup' else Result := '';
  end
  else if ModelPage.Values[1] then Result := '-Hosted'
  else Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  if CurUninstallStep <> usPostUninstall then Exit;
  // With no one to ask, as under a package manager, data stays.
  if UninstallSilent then Exit;
  if MsgBox('Delete your Hatchling data too? This removes the WSL distro {#Distro}, with your vault, conversations and memory. It can''t be undone.',
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    Exec('wsl.exe', '--unregister {#Distro}', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;
