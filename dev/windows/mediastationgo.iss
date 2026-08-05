#ifndef PackageSource
  #error PackageSource is required
#endif
#ifndef OutputDir
  #error OutputDir is required
#endif
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef OutputBaseFilename
  #define OutputBaseFilename "MediaStationGo-Setup"
#endif

[Setup]
AppId={{E8913718-46D8-45AB-8529-C7EF78CF024F}
AppName=MediaStationGo
AppVersion={#AppVersion}
AppPublisher=MediaStationGo
AppPublisherURL=https://github.com/timefunnel/MediaStationGo-Windows
AppSupportURL=https://github.com/timefunnel/MediaStationGo-Windows/issues
DefaultDirName={localappdata}\Programs\MediaStationGo
DefaultGroupName=MediaStationGo
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBaseFilename}
SetupIconFile=..\..\resources\win\mediastationgo.ico
UninstallDisplayIcon={app}\jellium-desktop.exe
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "{#PackageSource}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\MediaStationGo"; Filename: "{app}\jellium-desktop.exe"
Name: "{autodesktop}\MediaStationGo"; Filename: "{app}\jellium-desktop.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Run]
Filename: "{app}\jellium-desktop.exe"; Description: "{cm:LaunchProgram,MediaStationGo}"; Flags: nowait postinstall skipifsilent
