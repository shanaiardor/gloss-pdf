; 极简 PDF 的 Windows 安装包定义（Inno Setup 6）。
;
; 版本号由 CI 通过 ISCC 的 /DAPP_VERSION=... 传入，本地手工打包时可以省略，
; 会退回 0.0.0，避免误把开发构建当成正式版本发出去。

#ifndef APP_VERSION
  #define APP_VERSION "0.0.0"
#endif

#define AppName "极简 PDF"
#define AppExe "gpui-pdf.exe"
#define RepoUrl "https://github.com/shanaiardor/gpui-pdf"

[Setup]
; AppId 一经发布就不能改，否则升级会变成并存安装。
AppId={{A7C4F1D2-6B3E-4F2A-9C61-8E0D5B7A4F13}
AppName={#AppName}
AppVersion={#APP_VERSION}
AppVerName={#AppName} {#APP_VERSION}
AppPublisher=gpui-pdf
AppPublisherURL={#RepoUrl}
AppSupportURL={#RepoUrl}/issues
AppUpdatesURL={#RepoUrl}/releases
; 装到当前用户的 Programs 目录，自动更新时不需要 UAC 提权。
DefaultDirName={autopf}\gpui-pdf
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=gpui-pdf-{#APP_VERSION}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
UninstallDisplayIcon={app}\{#AppExe}
; 更新时允许安装器关闭正在运行的旧版本。
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
; pdfium.dll 必须和 exe 同目录，运行时按 exe 所在目录绑定。
Source: "..\pdfium.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
; 字体数据已经编进 exe，这里只放 OFL 要求的许可文本。
Source: "..\crates\gpui-pdf\assets\fonts\lxgw-wenkai-tc\OFL.txt"; DestDir: "{app}\third_party\lxgw-wenkai"; Flags: ignoreversion
Source: "..\third_party\pdfium\LICENSE"; DestDir: "{app}\third_party\pdfium"; Flags: ignoreversion
Source: "..\third_party\pdfium\VERSION"; DestDir: "{app}\third_party\pdfium"; Flags: ignoreversion
Source: "..\third_party\pdfium\licenses\*"; DestDir: "{app}\third_party\pdfium\licenses"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
