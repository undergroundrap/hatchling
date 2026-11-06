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
  #define Distro "Hatchling"
#endif
#define Script "powershell.exe"
#define Run "-NoProfile -ExecutionPolicy Bypass -File"

[Setup]
AppId={{7E0C6A52-4F0B-4B6E-9C3E-5A1D8E2B9F41}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Ocean Bennett
AppPublisherURL=https://github.com/undergroundrap/hatchling
DefaultDirName={autopf}\Hatchling
DefaultGroupName=Hatchling
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=Hatchling-{#AppVersion}-Setup
Compression=lzma
SolidCompression=yes
WizardStyle=modern
UninstallDisplayName=Hatchling
LicenseFile=..\..\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "common.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "setup.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "open.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "broker.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ReleaseDir}\*"; DestDir: "{app}\release"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Hatchling\Open Hatchling"; Filename: "{#Script}"; Parameters: "{#Run} ""{app}\open.ps1"" -Distro {#Distro}"
Name: "{autoprograms}\Hatchling\Protect your vault"; Filename: "{#Script}"; Parameters: "{#Run} ""{app}\broker.ps1"" -Distro {#Distro} -Do protect"
Name: "{autoprograms}\Hatchling\Make a backup"; Filename: "{#Script}"; Parameters: "{#Run} ""{app}\broker.ps1"" -Distro {#Distro} -Do backup"
Name: "{autoprograms}\Hatchling\Unlock"; Filename: "{#Script}"; Parameters: "{#Run} ""{app}\broker.ps1"" -Distro {#Distro} -Do unlock"
; WSL stops a distro soon after its last terminal closes: this keeps it running
; while the owner is logged in, with no window.
Name: "{userstartup}\Hatchling"; Filename: "{sys}\conhost.exe"; Parameters: "--headless wsl.exe -d {#Distro} --exec sleep infinity"; Tasks: keeprunning

[Tasks]
Name: "keeprunning"; Description: "Keep Hatchling running while I'm logged in (recommended)"

[Run]
Filename: "{#Script}"; Parameters: "{#Run} ""{app}\setup.ps1"" -Distro {#Distro} {code:ModelArgument}"; StatusMsg: "Setting Hatchling up in WSL. Follow the console window."; Flags: waituntilterminated

[UninstallRun]
Filename: "wsl.exe"; Parameters: "--terminate {#Distro}"; Flags: runhidden; RunOnceId: "StopDistro"

[Code]
var
  ModelPage: TInputOptionWizardPage;

procedure InitializeWizard;
begin
  ModelPage := CreateInputOptionPage(wpSelectTasks, 'Choose a model',
    'What Hatchling thinks with.',
    'You can switch later, in the app. Either way, Hatchling runs on this PC.', True, False);
  ModelPage.Add('On this PC, with Ollama: private, and needs a good GPU and about 7 GB');
  ModelPage.Add('Hosted, with OpenRouter: no GPU needed, and your messages go to a provider. You''ll need a key');
  ModelPage.Values[0] := True;
end;

function ModelArgument(Param: String): String;
begin
  if ModelPage.Values[1] then Result := '-Hosted' else Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  if CurUninstallStep <> usPostUninstall then Exit;
  if MsgBox('Delete your Hatchling data too? This removes the WSL distro {#Distro}, with your vault, conversations and memory. It can''t be undone.',
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    Exec('wsl.exe', '--unregister {#Distro}', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;
