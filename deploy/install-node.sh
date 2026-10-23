#!/usr/bin/env bash
# Run as root from the repository root after building aura-node on Linux.
set -euo pipefail
if [ "$(id -u)" -ne 0 ]; then
  printf 'Run this installer as root.\n' >&2
  exit 1
fi
test -x target/release/aura-node
getent passwd aura >/dev/null || useradd --system --home-dir /var/lib/aura-node --shell /usr/sbin/nologin aura
release="/opt/aura/releases/$(date -u +%Y%m%d-%H%M%S)"
install -d -m 755 "$release" /etc/aura
install -d -m 700 -o aura -g aura /var/lib/aura-node
install -m 755 target/release/aura-node "$release/aura-node"
if [ -f /etc/systemd/system/aura-node.service ]; then
  cp -a /etc/systemd/system/aura-node.service "$release/previous.service"
fi
install -m 644 deploy/aura-node.service /etc/systemd/system/aura-node.service
if [ ! -f /etc/aura/node.env ]; then
  install -m 644 deploy/node.env.example /etc/aura/node.env
fi
ln -sfn "$release" /opt/aura/current
systemctl daemon-reload
systemctl enable aura-node
systemctl restart aura-node
systemctl is-active aura-node
