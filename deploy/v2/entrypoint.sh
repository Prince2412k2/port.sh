#!/bin/sh
set -eu
mkdir -p /run/sshd
if [ ! -s /keys/ssh_host_ed25519_key ]; then
  ssh-keygen -q -t ed25519 -N '' -f /keys/ssh_host_ed25519_key
fi
exec /usr/sbin/sshd -D -e
