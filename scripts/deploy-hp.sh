#!/bin/bash
# Clean-install kvmshare on HP (Windows) over SSH.
# Binaries only: the state dir (%USERPROFILE%\.local\state\kvmshare) keeps
# its config/trust/identity — a reinstall must never make the user re-pair.
set -e
HP=ST@192.168.1.72
DIST=/home/bliss/Projects/kvmshare/dist/kvmshare_v0.0.0-dev_windows_amd64
SSH="ssh -o ConnectTimeout=8 -o BatchMode=yes $HP"
SCP="scp -o ConnectTimeout=8 -o BatchMode=yes"

echo "== 1. stop every kvmshare process =="
$SSH "taskkill /F /IM kvmshare-gui.exe /IM kvmshare-client.exe /IM kvmshare-server.exe /IM kvmshare-install.exe 2>nul & exit /b 0" || true

echo "== 2. remove old binaries + shortcuts (state kept) =="
$SSH "powershell -ExecutionPolicy Bypass -Command \"& { \$d = Join-Path \$env:LOCALAPPDATA 'kvmshare'; Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path \$d 'kvmshare-gui.exe'),(Join-Path \$d 'kvmshare-client.exe'),(Join-Path \$d 'kvmshare-server.exe'); Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path ([Environment]::GetFolderPath('Desktop')) 'kvmshare.lnk'); Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path \$env:APPDATA 'Microsoft\Windows\Start Menu\Programs\kvmshare.lnk') }\""

echo "== 3. upload fresh binaries =="
for f in kvmshare-gui.exe kvmshare-client.exe kvmshare-server.exe kvmshare-install.exe; do
  $SCP "$DIST/$f" "$HP:AppData/Local/kvmshare/$f"
done

echo "== 4. desktop + start-menu integration =="
$SSH "powershell -ExecutionPolicy Bypass -Command \"& { \$d = Join-Path \$env:LOCALAPPDATA 'kvmshare'; \$ws = New-Object -ComObject WScript.Shell; \$lnk = \$ws.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Desktop')) 'kvmshare.lnk')); \$lnk.TargetPath = (Join-Path \$d 'kvmshare-gui.exe'); \$lnk.Save(); \$sm = Join-Path \$env:APPDATA 'Microsoft\Windows\Start Menu\Programs'; \$lnk2 = \$ws.CreateShortcut((Join-Path \$sm 'kvmshare.lnk')); \$lnk2.TargetPath = (Join-Path \$d 'kvmshare-gui.exe'); \$lnk2.Save() }\""

echo "== 5. verify =="
$SSH "dir /b %LOCALAPPDATA%\\kvmshare\\*.exe"
echo "== launching GUI =="
$SSH "powershell -ExecutionPolicy Bypass -Command \"Start-Process (Join-Path \$env:LOCALAPPDATA 'kvmshare\kvmshare-gui.exe')\""
echo "HP clean install complete."
