#!/bin/sh
set -eu
export LANG=C.UTF-8 LC_ALL=C.UTF-8
export PORTFOLIO_V2_EPHEMERAL=1
# SSH_ORIGINAL_COMMAND is classified only, never evaluated or executed.
case "${SSH_ORIGINAL_COMMAND:-}" in
  "") exec /usr/local/bin/portfolio-v2-native --endpoint http://127.0.0.1:8322 ;;
  "resume "*)
    id=${SSH_ORIGINAL_COMMAND#resume }
    case "$id" in *[!0-9a-f]*) exit 1;; esac
    [ "${#id}" -eq 32 ] || exit 1
    exec /usr/local/bin/portfolio-v2-native --endpoint http://127.0.0.1:8322 --session "$id" ;;
  "mosh-server new"*|"mosh-server 'new'"*)
    exec /usr/bin/mosh-server new -s -c 256 -p 60100:60110 -- /usr/local/bin/portfolio-v2-native --endpoint http://127.0.0.1:8322 ;;
  *) printf '%s\n' 'This endpoint serves the portfolio client only.' >&2; exit 1 ;;
esac
