#!/bin/sh
set -eu
endpoint=${1:?Usage: install-v2.sh https://your-backend}
case "$endpoint" in https://*|http://*) ;; *) printf '%s\n' 'Expected an HTTP(S) backend URL' >&2; exit 1;; esac
case "$(uname -s)" in Linux) os=linux;; Darwin) os=darwin;; *) printf '%s\n' 'Download the Windows executable from the release artifacts.' >&2; exit 1;; esac
case "$(uname -m)" in x86_64|amd64) arch=amd64;; aarch64|arm64) arch=arm64;; *) printf '%s\n' 'Unsupported architecture' >&2; exit 1;; esac
artifact="portfolio-v2-native-$os-$arch"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
curl --fail --location --silent --show-error "${endpoint%/}/downloads/v2/$artifact" -o "$temporary/$artifact"
curl --fail --location --silent --show-error "${endpoint%/}/downloads/v2/SHA256SUMS" -o "$temporary/SHA256SUMS"
expected=$(awk -v name="$artifact" '$2 == name { print $1 }' "$temporary/SHA256SUMS")
test -n "$expected" || { printf '%s\n' 'Release checksum is missing' >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$temporary/$artifact" | cut -d ' ' -f 1)
else actual=$(shasum -a 256 "$temporary/$artifact" | cut -d ' ' -f 1); fi
test "$expected" = "$actual" || { printf '%s\n' 'Release checksum mismatch' >&2; exit 1; }
destination=${PORTFOLIO_V2_INSTALL_DIR:-"$HOME/.local/bin"}
mkdir -p "$destination"
install -m 755 "$temporary/$artifact" "$destination/portfolio-v2-native"
printf 'Installed %s/portfolio-v2-native\nConnect: %s/portfolio-v2-native --endpoint %s\n' "$destination" "$destination" "$endpoint"
