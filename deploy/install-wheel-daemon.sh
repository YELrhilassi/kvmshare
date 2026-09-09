#!/usr/bin/env bash
# kvmshare — one-time Linux install for the virtual wheel daemon.
#
# Installs, per session user:
#   ~/.local/bin/kvmshare-wheel-daemon   (the uinput wheel emitter)
#   ~/.config/systemd/user/kvmshare-wheel-daemon.service (auto-start)
# And once per machine (root, run via sudo):
#   /etc/udev/rules.d/70-kvmshare-uinput.rules (input-group access to /dev/uinput)
#
# The client spawns the daemon on demand if it is not running; the user
# service makes it always-on so even the first wheel event is smooth.
set -euo pipefail

PREFIX="${1:-$HOME/.local/bin}"
SERVICE_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "==> installing daemon binary to $PREFIX"
install -Dm755 "$SCRIPT_DIR/kvmshare-wheel-daemon" "$PREFIX/kvmshare-wheel-daemon"

echo "==> installing user service"
mkdir -p "$SERVICE_DIR"
cat > "$SERVICE_DIR/kvmshare-wheel-daemon.service" <<EOF
[Unit]
Description=kvmshare virtual wheel daemon (uinput)
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=$PREFIX/kvmshare-wheel-daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
EOF

systemctl --user daemon-reload
systemctl --user enable --now kvmshare-wheel-daemon.service

echo "==> udev rule (needs sudo once per machine)"
if [ "$(id -u)" -eq 0 ]; then
    install -Dm644 "$SCRIPT_DIR/70-kvmshare-uinput.rules" /etc/udev/rules.d/
    udevadm control --reload
    udevadm trigger /dev/uinput 2>/dev/null || true
    echo "udev rule installed."
else
    cat <<'NOTE'
Run this once as root on this machine (or let the client's first wheel
attempt prompt you for it via sudo):

  sudo install -Dm644 deploy/70-kvmshare-uinput.rules /etc/udev/rules.d/
  sudo udevadm control --reload && sudo udevadm trigger /dev/uinput
NOTE
fi

echo "==> done. Verify with:"
echo "    systemctl --user status kvmshare-wheel-daemon"
echo "    ls /run/user/$(id -u)/kvmshare-wheel-$(id -u).sock"
