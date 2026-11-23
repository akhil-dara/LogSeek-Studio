; Installer for LogSeek Studio (Inno Setup 6).
; Build:  iscc /DAppVersion=0.1.2 installer\logseek.iss
; The exe must already be built at target\release\logseek.exe.

#ifndef AppVersion
  #define AppVersion "0.1.2"
#endif

[Setup]
AppId={{6B2F3C0E-5D0A-4C55-9E0B-6C1B7B0E4A91}
AppName=LogSeek Studio
AppVersion={#AppVersion}
AppPublisher=LogSeek Studio
AppPublisherURL=https://github.com/akhil-dara/LogSeek-Studio
AppSupportURL=https://github.com/akhil-dara/LogSeek-Studio/issues
DefaultDirName={autopf}\LogSeek Studio
DefaultGroupName=LogSeek Studio
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE
SetupIconFile=..\assets\brand\logseek.ico
UninstallDisplayIcon={app}\logseek.exe
OutputDir=..\dist
OutputBaseFilename=LogSeek-Studio-{#AppVersion}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Installs for the current user without administrator rights by default,
; and offers "all users" on the first page for whoever wants that.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Put a shortcut on the desktop"; Flags: unchecked

[Files]
Source: "..\target\release\logseek.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\licenses\THIRD-PARTY-LICENSES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "README.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\LogSeek Studio"; Filename: "{app}\logseek.exe"
Name: "{autodesktop}\LogSeek Studio"; Filename: "{app}\logseek.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\logseek.exe"; Description: "Start LogSeek Studio"; Flags: nowait postinstall skipifsilent

; Indexes, tags and settings live under the user's own data folder and are
; left in place on uninstall: they belong to the user's files, not to the program.
