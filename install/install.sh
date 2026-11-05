#!/bin/sh
# Installs the prebuilt guard binary from a GitHub release.
#
# Usage (download the script, then run it):
#   sh install.sh
#
# Environment (all optional):
#   GUARD_VERSION      release to install, "0.1.0" or "v0.1.0" (default: latest)
#   GUARD_REPO         owner/name of the repository with the releases
#                      (default: CarlosBranty/journeygate)
#   GUARD_INSTALL_DIR  where the binary goes (default: $HOME/.local/bin)
#   GITHUB_TOKEN or GH_TOKEN
#                      sent as an Authorization header when set. Needed for a
#                      private repository; then the assets come from the REST API.
#
# Release assets: guard-<target>.tar.gz (the binary at the archive root) and
# guard-<target>.tar.gz.sha256 ("<hex>  <asset>"), built by
# .github/workflows/release.yml. The checksum is verified before extracting.
set -eu

repo="${GUARD_REPO:-CarlosBranty/journeygate}"
version="${GUARD_VERSION:-latest}"
version="${version#v}"
install_dir="${GUARD_INSTALL_DIR:-$HOME/.local/bin}"
token="${GITHUB_TOKEN:-${GH_TOKEN:-}}"

die() {
  echo "error: $*" >&2
  exit 1
}

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

os="$(uname -s)"
arch="$(uname -m)"
exe=guard
case "$os" in
  Linux)
    case "$arch" in
      x86_64 | amd64) target=x86_64-unknown-linux-gnu ;;
      aarch64 | arm64) target=aarch64-unknown-linux-gnu ;;
      *) die "unsupported Linux architecture: $arch" ;;
    esac
    ;;
  Darwin)
    case "$arch" in
      x86_64) target=x86_64-apple-darwin ;;
      arm64 | aarch64) target=aarch64-apple-darwin ;;
      *) die "unsupported macOS architecture: $arch" ;;
    esac
    ;;
  MINGW* | MSYS* | CYGWIN*)
    case "$arch" in
      x86_64 | amd64) target=x86_64-pc-windows-msvc ;;
      *) die "unsupported Windows architecture: $arch" ;;
    esac
    exe=guard.exe
    ;;
  *) die "unsupported operating system: $os (on Windows, use install.ps1)" ;;
esac

asset="guard-${target}.tar.gz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM

# With a token, curl reads the Authorization header from stdin (a here-doc,
# so the token never appears in curl's argv, which other local users can
# read) and does not follow redirects itself: an asset download answers with
# a redirect to the storage host, which is then fetched without the token
# (old curl, < 7.58, re-sent custom headers on redirects).
fetch() { # fetch <url> <out> [accept]
  if [ -n "$token" ]; then
    next="$(curl --fail --silent --show-error -H @- \
      -H "Accept: ${3:-application/vnd.github+json}" \
      -H "X-GitHub-Api-Version: 2022-11-28" \
      -w '%{redirect_url}' -o "$2" "$1" <<EOF
Authorization: Bearer ${token}
EOF
)" || return 1
    if [ -n "$next" ]; then
      curl --fail --silent --show-error --location -o "$2" "$next"
    fi
  else
    curl --fail --silent --show-error --location -o "$2" "$1"
  fi
}

# Prints the REST API url of the asset named $2 in the release JSON file $1.
# Each asset object lists "url" (.../releases/assets/<id>) before "name".
asset_api_url() {
  tr ',{}' '\n\n\n' <"$1" | awk -v want="$2" '
    /"url"[[:space:]]*:/ && /\/releases\/assets\// {
      u = $0; sub(/^[^:]*:[[:space:]]*"/, "", u); sub(/".*$/, "", u)
    }
    /"name"[[:space:]]*:/ {
      n = $0; sub(/^[^:]*:[[:space:]]*"/, "", n); sub(/".*$/, "", n)
      if (n == want && u != "") { print u; exit }
    }'
}

if [ "$version" = "latest" ]; then
  label="the latest release"
else
  label="v${version}"
fi
echo "installing guard ${label} (${target}) from ${repo}"

if [ -n "$token" ]; then
  # Private repositories: github.com download links need a browser session,
  # so resolve the assets through the REST API.
  if [ "$version" = "latest" ]; then
    api="https://api.github.com/repos/${repo}/releases/latest"
  else
    api="https://api.github.com/repos/${repo}/releases/tags/v${version}"
  fi
  fetch "$api" "$work/release.json" || die "cannot read ${label} of ${repo} (check the token's access)"
  for name in "$asset" "${asset}.sha256"; do
    url="$(asset_api_url "$work/release.json" "$name")"
    [ -n "$url" ] || die "${label} of ${repo} has no asset ${name}"
    fetch "$url" "$work/$name" application/octet-stream || die "cannot download ${name}"
  done
else
  if [ "$version" = "latest" ]; then
    base="https://github.com/${repo}/releases/latest/download"
  else
    base="https://github.com/${repo}/releases/download/v${version}"
  fi
  for name in "$asset" "${asset}.sha256"; do
    fetch "${base}/${name}" "$work/$name" ||
      die "cannot download ${base}/${name} (a private repository needs GITHUB_TOKEN or GH_TOKEN)"
  done
fi

# Verify, then extract: never run or unpack an archive whose checksum differs.
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$work" && sha256sum -c "${asset}.sha256" >/dev/null) || die "checksum mismatch for ${asset}"
elif command -v shasum >/dev/null 2>&1; then
  (cd "$work" && shasum -a 256 -c "${asset}.sha256" >/dev/null) || die "checksum mismatch for ${asset}"
else
  die "sha256sum or shasum is required to verify the download"
fi

mkdir -p "$work/out"
tar -xzf "$work/$asset" -C "$work/out"
[ -f "$work/out/$exe" ] || die "${asset} does not contain ${exe}"

mkdir -p "$install_dir"
cp "$work/out/$exe" "$install_dir/$exe"
chmod 0755 "$install_dir/$exe"
echo "installed $install_dir/$exe"
"$install_dir/$exe" --version

case ":${PATH}:" in
  *":${install_dir}:"*) ;;
  *)
    echo
    echo "$install_dir is not on your PATH. Add it, for example in ~/.profile:"
    echo "  export PATH=\"$install_dir:\$PATH\""
    ;;
esac
