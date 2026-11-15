; Опциональный шаблон Inno Setup для OB2H v2.0 (Фаза 76.2 PLAN_v2.0)
; ARP-регистрация, тихие флаги (/SILENT /VERYSILENT), делегирование деинсталляции в `ob2h self uninstall --silent`.

[Setup]
AppId={{8D6D47A0-4F5B-4B9E-9A51-OB2H00000200}
AppName=OB2H
AppVersion=2.0.0
AppPublisher=OB2H
DefaultDirName={localappdata}\ob2h
DefaultGroupName=OB2H
OutputBaseFilename=ob2h-setup-x86_64
Compression=lzma2
SolidCompression=yes
PrivilegesRequired=lowest

[Files]
Source: "..\target\release\ob2h.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Uninstall OB2H"; Filename: "{uninstallexe}"

[Run]
Filename: "{app}\ob2h.exe"; Parameters: "doctor"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\ob2h.exe"; Parameters: "self uninstall --keep-data --silent"; Flags: runhidden; RunOnceId: "Ob2hSelfUninstall"
