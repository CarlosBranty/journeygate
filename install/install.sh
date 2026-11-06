#!/bin/sh
# Installs guard: the prebuilt binary from a GitHub release (or a cargo build
# from source), then checks the tools a guard profile needs.
#
# Usage, from the published short URL (fetch it with `curl -fsSL <url>` and
# pipe it to sh; pass options after `sh -s --`):
#   sh install.sh [options]
#   sh -s -- --profile qa --yes          (the same options, from a pipe)
#
# Options (each one also has an environment variable):
#   --profile cli|qa|factory   GUARD_PROFILE      dependency profile
#                              (interactive: asked, default qa; otherwise cli)
#   --yes, -y                  GUARD_YES=1        no questions; accept installs
#                                                 that need no sudo
#   --allow-sudo               GUARD_ALLOW_SUDO=1 allow sudo without a question
#   --dry-run                  GUARD_DRY_RUN=1    print the plan; no network,
#                                                 no installs, no writes
#   --from-source              GUARD_FROM_SOURCE=1 build with cargo instead
#   --no-deps                  GUARD_NO_DEPS=1    the binary only (profile cli)
#   --help, -h                                    usage
#
# Environment (all optional):
#   GUARD_VERSION      release to install, "0.1.0" or "v0.1.0" (default: latest)
#   GUARD_REPO         owner/name of the repository with the releases
#                      (default: CarlosBranty/journeygate)
#   GUARD_INSTALL_DIR  where the binary goes (default: $HOME/.local/bin)
#   GITHUB_TOKEN or GH_TOKEN
#                      sent as an Authorization header when set. Needed for a
#                      private repository; then the assets come from the REST API.
#   GUARD_ASSET_FILE   a local guard-<target>.tar.gz to install instead of
#                      downloading one (offline installs)
#   GUARD_ASSET_SHA256 its expected sha256 hex digest (default: the first field
#                      of ${GUARD_ASSET_FILE}.sha256)
#   NO_COLOR           disable colors
#   GUARD_OS, GUARD_ARCH, GUARD_OS_RELEASE
#                      testing only: replace `uname -s`, `uname -m` and the
#                      /etc/os-release path
#
# Release assets: guard-<target>.tar.gz (the binary at the archive root) and
# guard-<target>.tar.gz.sha256 ("<hex>  <asset>"), built by
# .github/workflows/release.yml. The checksum (sha256sum or shasum -a 256) is
# verified before the archive is extracted.
#
# The whole script is the function main, called on the last line: the shell
# parses every line before it runs any, so a child process that consumes stdin
# cannot eat the rest of a piped script.
set -eu

die() {
  echo "error: $*" >&2
  exit 1
}

have() {
  command -v "$1" >/dev/null 2>&1
}

is_true() {
  case "${1:-}" in 1 | true | yes) return 0 ;; *) return 1 ;; esac
}

# Colors and marks, only on a terminal that wants them.
ui_init() {
  c_reset='' c_green='' c_yellow='' c_red='' c_dim='' c_cyan='' c_bold=''
  color=0
  tty_out=0
  [ -t 1 ] && tty_out=1
  if [ "$tty_out" = 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != dumb ]; then
    color=1
    esc="$(printf '\033')"
    c_reset="${esc}[0m" c_green="${esc}[32m" c_yellow="${esc}[33m"
    c_red="${esc}[31m" c_dim="${esc}[2m" c_cyan="${esc}[36m" c_bold="${esc}[1m"
  fi
  # Progress bars only on a terminal; scripts and CI get quiet curl.
  dl_progress=--silent
  [ "$tty_out" = 1 ] && dl_progress=--progress-bar
  # stdout, not stdin: under a pipe from curl, stdin is the script itself.
  interactive=0
  if [ "$tty_out" = 1 ] && [ "$yes" != 1 ] && [ -z "${CI:-}" ] && (: </dev/tty) 2>/dev/null; then
    interactive=1
  fi
}

say() {
  printf '%s\n' "$*"
}

# Indented detail under a step.
note() {
  printf '      %s\n' "$*"
}

mark() {
  if [ "$color" = 1 ]; then
    case "$1" in
      ok) printf '%s' "${c_green}✔${c_reset}" ;;
      warn) printf '%s' "${c_yellow}!${c_reset}" ;;
      fail) printf '%s' "${c_red}✖${c_reset}" ;;
      skip) printf '%s' "${c_dim}–${c_reset}" ;;
      plan) printf '%s' "${c_cyan}→${c_reset}" ;;
    esac
  else
    printf '%s' "$1"
  fi
}

step() { # step <n> <label> <mark> [detail]
  s_dots=''
  s_i=${#2}
  while [ "$s_i" -lt 24 ]; do
    s_dots="${s_dots}."
    s_i=$((s_i + 1))
  done
  printf '%s[%s/5]%s %s %s %s  %s\n' "$c_bold" "$1" "$c_reset" "$2" "$s_dots" "$(mark "$3")" "${4:-}"
}

# A mark line under a step: sub <mark> <text>
sub() {
  printf '      %s  %s\n' "$(mark "$1")" "$2"
}

ask_yn() { # ask_yn <question> <default y|n>; non-interactive: the default
  if [ "$interactive" != 1 ]; then
    [ "$2" = y ]
    return
  fi
  if [ "$2" = y ]; then a_hint='[Y/n]'; else a_hint='[y/N]'; fi
  printf '      %s %s ' "$1" "$a_hint"
  answer=''
  read -r answer </dev/tty || answer=''
  case "$answer" in
    [yY] | [yY][eE][sS]) return 0 ;;
    [nN] | [nN][oO]) return 1 ;;
    *) [ "$2" = y ] ;;
  esac
}

# With a token, curl reads the Authorization header from stdin (a here-doc,
# so the token never appears in curl's argv, which other local users can
# see) and does not follow redirects itself: an asset download answers with
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
      curl --fail "$dl_progress" --show-error --location -o "$2" "$next" </dev/null
    fi
  else
    curl --fail "$dl_progress" --show-error --location -o "$2" "$1" </dev/null
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

usage() {
  cat <<'USAGE'
guard installer

Usage:
  sh install.sh [options]
  sh -s -- [options]        when the script comes from curl -fsSL <url>

Options:
  --profile cli|qa|factory  what to check and offer to install besides guard
                              cli      the guard binary only
                              qa       + git, Node.js >= 24 (npm, npx),
                                       Playwright Chromium, pnpm (optional)
                              factory  + Docker with Compose v2, git >= 2.31,
                                       gh (optional), agent CLIs (reported)
                            default: asked (qa) on a terminal, cli otherwise
  --yes, -y                 no questions: accept installs that need no sudo
  --allow-sudo              allow sudo for package managers without a question
  --dry-run                 print the plan only: no network, no installs
  --from-source             build guard with cargo (offers rustup if needed)
  --no-deps                 install the binary only (same as --profile cli)
  --help, -h                this help

Environment: GUARD_PROFILE, GUARD_YES, GUARD_ALLOW_SUDO, GUARD_DRY_RUN,
GUARD_FROM_SOURCE, GUARD_NO_DEPS, GUARD_VERSION, GUARD_REPO, GUARD_INSTALL_DIR,
GITHUB_TOKEN or GH_TOKEN, GUARD_ASSET_FILE, GUARD_ASSET_SHA256, NO_COLOR.
USAGE
}

parse_args() {
  repo="${GUARD_REPO:-CarlosBranty/journeygate}"
  version="${GUARD_VERSION:-latest}"
  version="${version#v}"
  install_dir="${GUARD_INSTALL_DIR:-$HOME/.local/bin}"
  token="${GITHUB_TOKEN:-${GH_TOKEN:-}}"
  # Only fetch needs the token: keep it out of the environment of rustup,
  # cargo build scripts, npm/npx lifecycle scripts and package managers.
  unset GITHUB_TOKEN GH_TOKEN
  profile="${GUARD_PROFILE:-}"
  yes=0 allow_sudo=0 dry_run=0 from_source=0 no_deps=0
  is_true "${GUARD_YES:-}" && yes=1
  is_true "${GUARD_ALLOW_SUDO:-}" && allow_sudo=1
  is_true "${GUARD_DRY_RUN:-}" && dry_run=1
  is_true "${GUARD_FROM_SOURCE:-}" && from_source=1
  is_true "${GUARD_NO_DEPS:-}" && no_deps=1
  while [ $# -gt 0 ]; do
    case "$1" in
      --profile)
        [ $# -ge 2 ] || die "--profile needs a value: cli, qa or factory"
        profile="$2"
        shift
        ;;
      --profile=*) profile="${1#--profile=}" ;;
      --yes | -y) yes=1 ;;
      --allow-sudo) allow_sudo=1 ;;
      --dry-run) dry_run=1 ;;
      --from-source) from_source=1 ;;
      --no-deps) no_deps=1 ;;
      --help | -h)
        usage
        exit 0
        ;;
      *) die "unknown option: $1 (see --help)" ;;
    esac
    shift
  done
  case "$profile" in
    '' | cli | qa | factory) ;;
    *) die "unknown profile: $profile (cli, qa or factory)" ;;
  esac
  [ "$no_deps" = 1 ] && profile=cli
  return 0
}

detect_target() {
  os="${GUARD_OS:-$(uname -s)}"
  arch="${GUARD_ARCH:-$(uname -m)}"
  exe=guard
  target=''
  windows=0
  case "$os" in
    Linux)
      case "$arch" in
        x86_64 | amd64) target=x86_64-unknown-linux-gnu ;;
        aarch64 | arm64) target=aarch64-unknown-linux-gnu ;;
      esac
      ;;
    Darwin)
      case "$arch" in
        x86_64) target=x86_64-apple-darwin ;;
        arm64 | aarch64) target=aarch64-apple-darwin ;;
      esac
      ;;
    MINGW* | MSYS* | CYGWIN*)
      windows=1
      exe=guard.exe
      case "$arch" in
        x86_64 | amd64) target=x86_64-pc-windows-msvc ;;
      esac
      ;;
  esac
  return 0
}

choose_profile() {
  if [ -z "$profile" ]; then
    if [ "$interactive" = 1 ]; then
      say '      Which tools should the installer check for you?'
      say '        1) cli      the guard binary only'
      say '        2) qa       + git, Node.js >= 24, Playwright Chromium (recommended)'
      say '        3) factory  + Docker with Compose v2, gh, agent CLIs'
      printf '      Profile [2]: '
      answer=''
      read -r answer </dev/tty || answer=''
      case "$answer" in
        1 | cli) profile=cli ;;
        3 | factory) profile=factory ;;
        *) profile=qa ;;
      esac
    else
      profile=cli
    fi
  fi
  step 2 'Choose profile' ok "$profile"
}

# Downloads the archive and its checksum into $work. Returns 1 on failure.
download_release() {
  if [ -n "$token" ]; then
    # Private repositories: github.com download links need a browser session,
    # so resolve the assets through the REST API.
    if [ "$version" = "latest" ]; then
      api="https://api.github.com/repos/${repo}/releases/latest"
    else
      api="https://api.github.com/repos/${repo}/releases/tags/v${version}"
    fi
    if [ "$dry_run" = 1 ]; then
      plan_detail="GET ${api} (with your token), then ${asset} and ${asset}.sha256"
      return 0
    fi
    fetch "$api" "$work/release.json" || {
      download_error="cannot find ${label} of ${repo} (check the token's access, or that a release exists)"
      return 1
    }
    for name in "$asset" "${asset}.sha256"; do
      url="$(asset_api_url "$work/release.json" "$name")"
      [ -n "$url" ] || {
        download_error="${label} of ${repo} has no asset ${name}"
        return 1
      }
      fetch "$url" "$work/$name" application/octet-stream || {
        download_error="cannot download ${name}"
        return 1
      }
    done
  else
    if [ "$version" = "latest" ]; then
      base="https://github.com/${repo}/releases/latest/download"
    else
      base="https://github.com/${repo}/releases/download/v${version}"
    fi
    if [ "$dry_run" = 1 ]; then
      plan_detail="download ${base}/${asset} and ${asset}.sha256"
      return 0
    fi
    for name in "$asset" "${asset}.sha256"; do
      fetch "${base}/${name}" "$work/$name" || {
        download_error="cannot download ${base}/${name} (no release yet? a private repository needs GITHUB_TOKEN or GH_TOKEN)"
        return 1
      }
    done
  fi
  return 0
}

# GUARD_ASSET_FILE: copy the local archive into $work and write its checksum
# file, so it goes through the same verify-then-extract code as a download.
use_local_asset() {
  [ -f "$GUARD_ASSET_FILE" ] || die "GUARD_ASSET_FILE does not exist: $GUARD_ASSET_FILE"
  hex="${GUARD_ASSET_SHA256:-}"
  if [ -z "$hex" ]; then
    [ -f "${GUARD_ASSET_FILE}.sha256" ] ||
      die "no checksum for $GUARD_ASSET_FILE: set GUARD_ASSET_SHA256 or put ${GUARD_ASSET_FILE}.sha256 next to it"
    hex="$(awk 'NR == 1 { print $1 }' "${GUARD_ASSET_FILE}.sha256")"
  fi
  hex="$(printf '%s' "$hex" | tr 'A-F' 'a-f')"
  case "$hex" in
    '' | *[!0-9a-f]*) die "the expected checksum is not a hex sha256 digest" ;;
  esac
  if [ "$dry_run" = 1 ]; then
    plan_detail="use ${GUARD_ASSET_FILE}, verify sha256 ${hex}"
    return 0
  fi
  cp "$GUARD_ASSET_FILE" "$work/$asset"
  printf '%s  %s\n' "$hex" "$asset" >"$work/${asset}.sha256"
}

# Verify, then extract: never run or unpack an archive whose checksum differs.
# The one extraction site for downloads and GUARD_ASSET_FILE alike.
verify_and_extract() {
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$work" && sha256sum -c "${asset}.sha256" >/dev/null 2>&1) || die "checksum mismatch for ${asset}"
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$work" && shasum -a 256 -c "${asset}.sha256" >/dev/null 2>&1) || die "checksum mismatch for ${asset}"
  else
    die "sha256sum or shasum is required to verify the download"
  fi

  mkdir -p "$work/out"
  tar -xzf "$work/$asset" -C "$work/out"
  [ -f "$work/out/$exe" ] || die "${asset} does not contain ${exe}"
  built="$work/out/$exe"
}

# The first dotted version number on stdin.
ver_of() {
  sed -n 's/^[^0-9]*\([0-9][0-9]*\(\.[0-9][0-9]*\)*\).*/\1/p' | head -n 1
}

# version_ge <a> <b>: true when dotted version a >= b.
version_ge() {
  vg_a="$1" vg_b="$2"
  while [ -n "$vg_a" ] || [ -n "$vg_b" ]; do
    vg_x="${vg_a%%.*}" vg_y="${vg_b%%.*}"
    if [ "$vg_x" = "$vg_a" ]; then vg_a=''; else vg_a="${vg_a#*.}"; fi
    if [ "$vg_y" = "$vg_b" ]; then vg_b=''; else vg_b="${vg_b#*.}"; fi
    case "$vg_x" in '' | *[!0-9]*) vg_x=0 ;; esac
    case "$vg_y" in '' | *[!0-9]*) vg_y=0 ;; esac
    [ "$vg_x" -gt "$vg_y" ] && return 0
    [ "$vg_x" -lt "$vg_y" ] && return 1
  done
  return 0
}

probe() { # probe <cmd> [args]: its version, or nothing
  have "$1" || return 0
  "$@" </dev/null 2>/dev/null | ver_of || true
}

detect_rust() {
  rustc_ver="$(probe rustc --version)"
  cargo_ok=0
  have cargo && cargo_ok=1
  return 0
}

download_rustup() {
  curl --proto '=https' --tlsv1.2 -fsS -o "$work/rustup-init.sh" https://sh.rustup.rs </dev/null
}

ensure_rust() {
  detect_rust
  if [ "$cargo_ok" = 1 ] && [ -n "$rustc_ver" ] && version_ge "$rustc_ver" 1.88; then
    sub ok "rustc $rustc_ver, cargo"
    return 0
  fi
  if [ "$cargo_ok" = 1 ] && [ -n "$rustc_ver" ] && have rustup; then
    sub warn "rustc $rustc_ver is older than 1.88 (Cargo.toml rust-version)"
    run_install Rust '' rustup update stable
  elif [ "$cargo_ok" = 1 ] && [ -n "$rustc_ver" ]; then
    die "rustc $rustc_ver is older than 1.88 and was not installed with rustup; update it, then rerun"
  else
    sub warn "Rust (cargo, rustc >= 1.88) is missing"
    rustup_show="curl --proto '=https' --tlsv1.2 -fsS -o $work/rustup-init.sh https://sh.rustup.rs && sh $work/rustup-init.sh -y --profile minimal"
    if consent 'Rust (official rustup)' '' "$rustup_show"; then
      download_rustup || die "cannot download https://sh.rustup.rs"
      sh "$work/rustup-init.sh" -y --profile minimal </dev/null 2>&1 | sed 's/^/        /'
      PATH="$HOME/.cargo/bin:$PATH"
      export PATH
      ri_result=installed
    fi
  fi
  [ "$dry_run" = 1 ] && return 0
  detect_rust
  if [ "$cargo_ok" = 1 ] && [ -n "$rustc_ver" ] && version_ge "$rustc_ver" 1.88; then
    sub ok "rustc $rustc_ver, cargo"
    return 0
  fi
  die "building from source needs Rust >= 1.88: rerun with --yes to install it with rustup, or install it from https://rustup.rs"
}

# The MSVC C++ build tools, found the way rustup finds them (vswhere).
msvc_tools() {
  mt_pf="$(printenv 'ProgramFiles(x86)' 2>/dev/null || true)"
  [ -n "$mt_pf" ] || return 1
  have cygpath && mt_pf="$(cygpath -u "$mt_pf")"
  mt_vswhere="$mt_pf/Microsoft Visual Studio/Installer/vswhere.exe"
  [ -f "$mt_vswhere" ] || return 1
  [ -n "$("$mt_vswhere" -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath </dev/null 2>/dev/null)" ]
}

# cargo links through the platform C toolchain, and ring and libsqlite3-sys
# compile C code: without it the build fails with "linker `cc` not found".
ensure_cc() {
  case "$os" in
    Darwin)
      # /usr/bin/cc exists without the tools (a shim): ask xcode-select.
      if xcode-select -p >/dev/null 2>&1; then
        sub ok "Xcode Command Line Tools"
        return 0
      fi
      sub warn "the Xcode Command Line Tools (C compiler and linker) are missing"
      run_install 'Xcode Command Line Tools' '' xcode-select --install
      [ "$dry_run" = 1 ] && return 0
      xcode-select -p >/dev/null 2>&1 ||
        die "building from source needs the Xcode Command Line Tools: finish their installer (xcode-select --install), then rerun"
      ;;
    MINGW* | MSYS* | CYGWIN*)
      if msvc_tools; then
        sub ok "MSVC Build Tools (C++ workload)"
        return 0
      fi
      sub warn "the MSVC Build Tools (C++ workload) are required to build Rust code on Windows"
      if [ "$pm" = winget ]; then
        # shellcheck disable=SC2086 # winget_flags is a word list
        run_install 'MSVC Build Tools' '' winget install -e --id Microsoft.VisualStudio.2022.BuildTools $winget_flags \
          --override '--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
      else
        ic_cmd=''
        offer 'MSVC Build Tools' https://visualstudio.microsoft.com/visual-cpp-build-tools/
      fi
      [ "$dry_run" = 1 ] && return 0
      msvc_tools ||
        die "building from source on Windows needs the MSVC Build Tools (C++ workload): install them, then rerun"
      ;;
    *)
      if have cc; then
        sub ok "C toolchain (cc)"
        return 0
      fi
      sub warn "a C compiler and linker (cc) are missing"
      cmd_for cc
      offer 'C toolchain' 'your package manager: build-essential (Debian, Ubuntu), gcc (Fedora), base-devel (Arch)'
      [ "$dry_run" = 1 ] && return 0
      have cc || die "building from source needs a C compiler and linker (cc): install one, then rerun"
      ;;
  esac
  return 0
}

build_from_source() {
  if [ "$dry_run" = 1 ]; then s3m=plan; else s3m=ok; fi
  step 3 'Build from source' "$s3m" "cargo install from https://github.com/${repo}"
  ensure_rust
  ensure_cc
  set -- cargo install --locked --root "$work/cargo" --git "https://github.com/${repo}"
  if [ "$version" != latest ]; then
    set -- "$@" --tag "v${version}"
  else
    note "no version given: building the default branch"
  fi
  set -- "$@" guard-cli --bin guard
  note "a private repository uses your git credentials (e.g. gh auth setup-git); no token is passed to cargo"
  if [ "$dry_run" = 1 ]; then
    sub plan "$*"
    return 0
  fi
  "$@" </dev/null 2>&1 | sed 's/^/        /'
  built="$work/cargo/bin/$exe"
  [ -f "$built" ] || die "cargo did not build $exe (see the output above)"
  sub ok "built $exe"
}

install_binary() {
  if [ "$dry_run" = 1 ]; then
    step 4 'Install binary' plan "$install_dir/$exe"
    return 0
  fi
  mkdir -p "$install_dir"
  # Never write into the existing binary: a running guard (guard mcp,
  # guard-gui) makes that fail, and on macOS it breaks the code signature.
  # Copy next to it, then rename over it; Windows cannot rename over a
  # running exe, but it can move that one aside first.
  ib_dest="$install_dir/$exe"
  ib_new="$install_dir/.$exe.new"
  ib_hint="cannot replace $ib_dest; stop running guard (guard mcp, guard-gui) and rerun"
  cp "$built" "$ib_new" || die "cannot write $ib_new"
  chmod 0755 "$ib_new"
  if [ "$windows" = 1 ] && [ -e "$ib_dest" ]; then
    rm -f "$ib_dest.old" 2>/dev/null || true
    if [ ! -e "$ib_dest.old" ]; then
      mv -f "$ib_dest" "$ib_dest.old" 2>/dev/null || true
    fi
  fi
  mv -f "$ib_new" "$ib_dest" || {
    rm -f "$ib_new"
    [ -e "$ib_dest" ] || [ ! -e "$ib_dest.old" ] || mv -f "$ib_dest.old" "$ib_dest" || true
    die "$ib_hint"
  }
  guard_version="$("$install_dir/$exe" --version </dev/null)" || die "$install_dir/$exe --version failed"
  step 4 'Install binary' ok "$install_dir/$exe"
  echo "installed $install_dir/$exe"
}

pkg_manager() {
  pm=''
  case "$os" in
    Darwin) have brew && pm=brew ;;
    Linux)
      osr="${GUARD_OS_RELEASE:-/etc/os-release}"
      ids=''
      if [ -f "$osr" ]; then
        ids="$(sed -n -e 's/^ID=//p' -e 's/^ID_LIKE=//p' "$osr" | tr -d '"' | tr '\n' ' ')"
      fi
      for i in $ids; do
        case "$i" in
          debian | ubuntu) have apt-get && pm=apt ;;
          fedora | rhel | centos) have dnf && pm=dnf ;;
          arch) have pacman && pm=pacman ;;
        esac
        [ -n "$pm" ] && break
      done
      ;;
    MINGW* | MSYS* | CYGWIN*) have winget && pm=winget ;;
  esac
  return 0
}

uid() {
  id -u 2>/dev/null || echo 1
}

# consent <dep> <needs_root: ''|sudo|self> <shown command>
# Returns 0 when the command may run now. In dry-run it prints the plan and
# returns 1. Sets ri_result: planned | skipped.
consent() {
  cs_dep="$1" cs_root="$2" cs_show="$3"
  cs_sudo=0
  if [ -n "$cs_root" ] && [ "$(uid)" != 0 ]; then
    cs_sudo=1
    [ "$cs_root" = sudo ] && cs_show="sudo $cs_show"
  fi
  if [ "$dry_run" = 1 ]; then
    sub plan "$cs_show"
    if [ "$cs_sudo" = 1 ]; then
      note "needs administrator rights (sudo)"
      [ "$interactive" = 1 ] || [ "$allow_sudo" = 1 ] ||
        note "non-interactive: runs only with --allow-sudo"
    fi
    ri_result=planned
    return 1
  fi
  if [ "$cs_sudo" = 1 ]; then
    if [ "$cs_root" = sudo ]; then
      note "This needs administrator rights: $cs_show"
    else
      note "This needs administrator rights: sudo (it calls sudo itself): $cs_show"
    fi
  fi
  if [ "$interactive" = 1 ]; then
    if ! ask_yn "Install $cs_dep with \"$cs_show\"?" n; then
      skipped "$cs_dep" "$cs_show"
      return 1
    fi
    if [ "$cs_sudo" = 1 ] && ! ask_yn "Run it with sudo?" n; then
      skipped "$cs_dep" "$cs_show"
      return 1
    fi
  else
    if [ "$yes" != 1 ]; then
      note "skipped (non-interactive; rerun with --yes to install it)"
      skipped "$cs_dep" "$cs_show"
      return 1
    fi
    if [ "$cs_sudo" = 1 ] && [ "$allow_sudo" != 1 ]; then
      note "skipped: needs sudo; rerun with --allow-sudo or run it yourself"
      skipped "$cs_dep" "$cs_show"
      return 1
    fi
  fi
  if [ "$pm" = brew ] && [ "$(uid)" = 0 ]; then
    note "skipped: brew must not run as root"
    skipped "$cs_dep" "$cs_show"
    return 1
  fi
  return 0
}

# run_install <dep> <needs_root: ''|sudo|self> <cmd...>
# Never fails the script: a failed dependency install is a warning.
run_install() {
  ri_dep="$1" ri_root="$2"
  shift 2
  ri_result=skipped
  consent "$ri_dep" "$ri_root" "$*" || return 0
  case "$1" in winget | winget.exe) note "winget may show a Windows UAC prompt" ;; esac
  if [ "$ri_root" = sudo ] && [ "$(uid)" != 0 ]; then
    set -- sudo "$@"
  fi
  ri_rc="$work/install.rc"
  # Only the status echoes write to $ri_rc; the command's own output goes
  # through the indenting sed.
  if [ "$interactive" = 1 ]; then
    { if "$@" </dev/tty; then echo 0 >"$ri_rc"; else echo 1 >"$ri_rc"; fi; } 2>&1 | sed 's/^/        /'
  else
    { if "$@" </dev/null; then echo 0 >"$ri_rc"; else echo 1 >"$ri_rc"; fi; } 2>&1 | sed 's/^/        /'
  fi
  if [ "$(sed -n 1p "$ri_rc" 2>/dev/null)" = 0 ]; then
    ri_result=installed
  else
    ri_result=failed
    sub warn "installing $ri_dep failed; run it yourself: $*"
    skipped "$ri_dep" "$*"
  fi
  return 0
}

skipped() { # skipped <dep> <command>
  skip_rows="${skip_rows}  $1: $2
"
}

status_of() { # status_of <found> <text when missing>
  if [ -n "$1" ]; then printf ok; else printf '%s' "$2"; fi
}

record() { # record <dep> <status> <found>
  dep_rows="${dep_rows}$(printf '  %-18s %-22s %s' "$1" "$2" "$3")
"
}

# offer <dep> <url>: install with $ic_cmd/$ic_root, or point at $url.
offer() {
  if [ -n "$ic_cmd" ]; then
    case "$ic_cmd" in apt-get\ install*) apt_refresh ;; esac
    # shellcheck disable=SC2086 # ic_cmd is a word list built by this script
    run_install "$1" "$ic_root" $ic_cmd
  else
    if [ "$dry_run" = 1 ]; then
      sub plan "install it yourself: $2"
      ri_result=planned
    else
      note "install it yourself: $2"
      skipped "$1" "see $2"
      ri_result=skipped
    fi
  fi
}

# The installer's own question is the consent, so winget must not stop on its
# source or package agreements (it would hang, or fail with --yes).
winget_flags='--source winget --accept-source-agreements --accept-package-agreements'
# -Syu: a sync without the upgrade is a partial upgrade, which Arch does not
# support; --needed keeps packages that are already current.
pacman_sync='-Syu --needed --noconfirm'

# apt-get install fails on empty package lists (fresh containers) and stale
# ones (404s): refresh them once, asked like any other sudo command.
apt_refresh() {
  [ "$pm" = apt ] && [ "$apt_tried" != 1 ] || return 0
  apt_tried=1
  ar_result="$ri_result"
  run_install 'apt package lists' sudo apt-get update
  ri_result="$ar_result"
}

# Sets ic_cmd and ic_root for a dependency on this package manager.
cmd_for() {
  ic_cmd='' ic_root=''
  case "$pm" in apt | dnf | pacman) ic_root=sudo ;; esac
  case "$1:$pm" in
    git:brew) ic_cmd='brew install git' ;;
    git:apt) ic_cmd='apt-get install -y git' ;;
    git:dnf) ic_cmd='dnf install -y git' ;;
    git:pacman) ic_cmd="pacman $pacman_sync git" ;;
    git:winget) ic_cmd="winget install -e --id Git.Git $winget_flags" ;;
    node:brew) ic_cmd='brew install node' ;;
    node:apt)
      if [ "$dry_run" = 1 ]; then
        ic_cmd='apt-get install -y nodejs'
        node_note='only if apt-cache policy nodejs offers >= 24, else https://nodejs.org/en/download; npm too when the nodejs package lacks it'
      else
        # The candidate is only known with fresh package lists.
        apt_refresh
        nc="$(apt-cache policy nodejs </dev/null 2>/dev/null | sed -n 's/^ *Candidate: *//p' | head -n 1)"
        nc="${nc#*:}"
        case "${nc%%.*}" in '' | *[!0-9]*) ;; *) [ "${nc%%.*}" -ge 24 ] && ic_cmd='apt-get install -y nodejs' ;; esac
      fi
      ;;
    node:dnf)
      if [ "$dry_run" = 1 ]; then
        ic_cmd='dnf install -y nodejs'
        node_note='only if dnf info nodejs shows >= 24, else https://nodejs.org/en/download'
      else
        nc="$(dnf info nodejs </dev/null 2>/dev/null | sed -n 's/^Version *: *//p' | head -n 1)"
        case "${nc%%.*}" in '' | *[!0-9]*) ;; *) [ "${nc%%.*}" -ge 24 ] && ic_cmd='dnf install -y nodejs' ;; esac
      fi
      ;;
    node:pacman) ic_cmd="pacman $pacman_sync nodejs npm" ;;
    node:winget) ic_cmd="winget install -e --id OpenJS.NodeJS.LTS $winget_flags" ;;
    # Debian and Ubuntu ship npm (and npx) apart from nodejs; NodeSource's
    # nodejs includes it, so this is only offered when npm is still missing.
    npm:apt) ic_cmd='apt-get install -y npm' ;;
    npm:dnf) ic_cmd='dnf install -y npm' ;;
    npm:pacman) ic_cmd="pacman $pacman_sync npm" ;;
    # The C compiler and linker cargo needs (ring and libsqlite3-sys compile C).
    cc:apt) ic_cmd='apt-get install -y build-essential' ;;
    cc:dnf) ic_cmd='dnf install -y gcc' ;;
    cc:pacman) ic_cmd="pacman $pacman_sync base-devel" ;;
    pnpm:brew) ic_cmd='brew install pnpm' ;;
    pnpm:winget) ic_cmd="winget install -e --id pnpm.pnpm $winget_flags" ;;
    pnpm:*)
      ic_root=''
      if have npm; then
        if [ "$dry_run" = 1 ]; then
          ic_cmd='npm install -g pnpm@9'
        else
          np="$(npm prefix -g </dev/null 2>/dev/null || true)"
          if [ -n "$np" ] && { [ -w "$np/lib" ] || { [ ! -d "$np/lib" ] && [ -w "$np" ]; }; }; then
            ic_cmd='npm install -g pnpm@9'
          fi
        fi
      fi
      ;;
    docker:brew) ic_cmd='brew install --cask docker' ;;
    docker:pacman) ic_cmd="pacman $pacman_sync docker docker-compose" ;;
    docker:winget) ic_cmd="winget install -e --id Docker.DockerDesktop $winget_flags" ;;
    gh:brew) ic_cmd='brew install gh' ;;
    gh:apt) ic_cmd='apt-get install -y gh' ;;
    gh:dnf) ic_cmd='dnf install -y gh' ;;
    gh:pacman) ic_cmd="pacman $pacman_sync github-cli" ;;
    gh:winget) ic_cmd="winget install -e --id GitHub.cli $winget_flags" ;;
  esac
  [ -n "$ic_cmd" ] || ic_root=''
  return 0
}

pw_cache_dir() {
  case "$os" in
    Darwin) printf '%s' "$HOME/Library/Caches/ms-playwright" ;;
    MINGW* | MSYS* | CYGWIN*)
      if [ -n "${LOCALAPPDATA:-}" ]; then
        if have cygpath; then
          printf '%s' "$(cygpath -u "$LOCALAPPDATA")/ms-playwright"
        else
          printf '%s' "$LOCALAPPDATA/ms-playwright"
        fi
      fi
      ;;
    *) printf '%s' "${XDG_CACHE_HOME:-$HOME/.cache}/ms-playwright" ;;
  esac
}

detect_git() {
  git_ver="$(probe git --version)"
  return 0
}

detect_node() {
  node_ver="$(probe node --version)"
  npm_ver="$(probe npm --version)"
  npx_ver="$(probe npx --version)"
  return 0
}

detect_pnpm() {
  pnpm_ver="$(probe pnpm --version)"
  return 0
}

detect_chromium() {
  chromium_dir=''
  # Playwright uses PLAYWRIGHT_BROWSERS_PATH alone when it is set.
  pw_dir="${PLAYWRIGHT_BROWSERS_PATH:-$(pw_cache_dir)}"
  for d in "$pw_dir"; do
    [ -n "$d" ] && [ "$d" != 0 ] && [ -d "$d" ] || continue
    for c in "$d"/chromium-*; do
      if [ -d "$c" ]; then
        chromium_dir="$c"
        return 0
      fi
    done
  done
  return 0
}

detect_docker() {
  docker_ver="$(probe docker --version)"
  compose_ver=''
  docker_up=0
  if [ -n "$docker_ver" ]; then
    compose_ver="$(probe docker compose version)"
    docker info </dev/null >/dev/null 2>&1 && docker_up=1
  fi
  return 0
}

detect_gh() {
  gh_ver="$(probe gh --version)"
  return 0
}

detect_agents() {
  agents_found=''
  for a in claude codex; do
    have "$a" && agents_found="${agents_found}${agents_found:+, }$a"
  done
  return 0
}

after_install() { # after_install <dep> <required|recommended|optional>
  case "$ri_result" in
    planned) record "$1" "missing (planned)" '' ;;
    installed) ;;
    *) record "$1" "missing ($2)" '' ;;
  esac
}

check_git() {
  git_min=''
  [ "$profile" = factory ] && git_min=2.31
  detect_git
  if [ -n "$git_ver" ] && { [ -z "$git_min" ] || version_ge "$git_ver" "$git_min"; }; then
    sub ok "git $git_ver"
    record git ok "$git_ver"
    return 0
  fi
  if [ -n "$git_ver" ]; then
    sub warn "git $git_ver: needs >= $git_min for the PR publisher"
  else
    sub warn "git: missing (required: doctor, affected, suspects, gate finalize)"
  fi
  cmd_for git
  offer git https://git-scm.com/downloads
  if [ "$ri_result" = installed ]; then
    detect_git
    record git "$(status_of "$git_ver" 'missing (required)')" "$git_ver"
  else
    after_install git required
  fi
}

check_node() {
  detect_node
  node_major="${node_ver%%.*}"
  if [ -n "$node_ver" ] && version_ge "$node_ver" 24; then
    sub ok "node $node_ver, npm ${npm_ver:-missing}, npx ${npx_ver:-missing}"
    record node ok "$node_ver"
  else
    if [ -n "$node_ver" ]; then
      sub warn "node $node_ver: needs >= 24 (major $node_major)"
    else
      sub warn "node: missing (required: needs >= 24 with npm and npx)"
    fi
    node_note=''
    cmd_for node
    offer 'Node.js' https://nodejs.org/en/download
    [ -n "$node_note" ] && note "$node_note"
    if [ "$ri_result" = installed ]; then
      detect_node
      if [ -n "$node_ver" ] && version_ge "$node_ver" 24; then
        record node ok "$node_ver"
      else
        record node "needs >= 24" "$node_ver"
      fi
    elif [ -n "$node_ver" ]; then
      record node "needs >= 24" "$node_ver"
    else
      after_install node required
    fi
  fi
  # node without npm: the distro split them into two packages.
  if [ -n "$node_ver" ] && [ -z "$npm_ver" ]; then
    sub warn "npm: missing (required: guard runs npm and npx)"
    cmd_for npm
    offer npm https://docs.npmjs.com/downloading-and-installing-node-js-and-npm
    if [ "$ri_result" = installed ]; then detect_node; fi
  fi
  record npm "$(status_of "$npm_ver" 'missing (required)')" "$npm_ver"
  record npx "$(status_of "$npx_ver" 'missing (required)')" "$npx_ver"
}

check_chromium() {
  detect_chromium
  if [ -n "$chromium_dir" ]; then
    sub ok "Playwright Chromium ${chromium_dir##*/}"
    record chromium ok "${chromium_dir##*/}"
    case "$os" in Linux) note "if Chromium fails to start: npx -y playwright@1.64.1 install-deps chromium" ;; esac
    return 0
  fi
  sub warn "Playwright Chromium: missing (recommended for guard test)"
  if [ "$dry_run" != 1 ] && ! have npx; then
    note "needs Node.js and npx first; then run: npx -y playwright@1.64.1 install chromium"
    skipped chromium 'npx -y playwright@1.64.1 install chromium'
    record chromium 'missing (recommended)' ''
    return 0
  fi
  run_install 'Playwright Chromium' '' npx -y playwright@1.64.1 install chromium
  ch_result="$ri_result"
  if [ "$ch_result" = installed ] || [ "$ch_result" = planned ]; then
    case "$os" in
      Linux) run_install 'Chromium system libraries' self npx -y playwright@1.64.1 install-deps chromium ;;
    esac
  fi
  if [ "$ch_result" = installed ]; then
    detect_chromium
    record chromium ok "${chromium_dir##*/}"
  else
    ri_result="$ch_result"
    after_install chromium recommended
  fi
}

check_pnpm() {
  detect_pnpm
  if [ -n "$pnpm_ver" ]; then
    sub ok "pnpm $pnpm_ver"
    record pnpm ok "$pnpm_ver"
    return 0
  fi
  sub skip "pnpm: missing (optional: gate finalize with a pnpm-lock.yaml)"
  cmd_for pnpm
  offer pnpm https://pnpm.io/installation
  if [ "$ri_result" = installed ]; then
    detect_pnpm
    record pnpm "$(status_of "$pnpm_ver" 'missing (optional)')" "$pnpm_ver"
  else
    after_install pnpm optional
  fi
}

check_docker() {
  detect_docker
  if [ -n "$docker_ver" ] && [ -n "$compose_ver" ]; then
    sub ok "docker $docker_ver, compose $compose_ver"
    record docker ok "$docker_ver"
    record 'docker compose' ok "$compose_ver"
    if [ "$docker_up" != 1 ]; then
      sub warn "the Docker daemon is not running; start it before guard test --docker"
    fi
    return 0
  fi
  if [ -n "$docker_ver" ]; then
    sub warn "docker $docker_ver without Compose v2 (docker compose)"
  else
    sub warn "docker: missing (required: test --docker, repro, agent lane, deploy stack)"
  fi
  cmd_for docker
  [ "$pm" = brew ] && note "Docker Desktop needs a paid subscription in larger companies (docker.com/pricing)"
  offer 'Docker with Compose v2' https://docs.docker.com/engine/install/
  case "$os" in
    Linux) note "then let your user run it: sudo usermod -aG docker \$USER (log out and back in)" ;;
  esac
  if [ "$ri_result" = installed ]; then
    detect_docker
    record docker "$(status_of "$docker_ver" 'missing (required)')" "$docker_ver"
    record 'docker compose' "$(status_of "$compose_ver" 'missing (required)')" "$compose_ver"
  else
    after_install docker required
  fi
}

check_gh() {
  detect_gh
  if [ -n "$gh_ver" ]; then
    sub ok "gh $gh_ver"
    record gh ok "$gh_ver"
    return 0
  fi
  sub skip "gh: missing (optional: GITHUB_TOKEN=\"\$(gh auth token)\" for private repositories)"
  cmd_for gh
  offer gh https://cli.github.com
  if [ "$ri_result" = installed ]; then
    detect_gh
    record gh "$(status_of "$gh_ver" 'missing (optional)')" "$gh_ver"
  else
    after_install gh optional
  fi
}

deps_for_profile() {
  if [ "$profile" = cli ]; then
    step 5 'Check dependencies' skip 'profile cli'
    note "rerun with --profile qa to check git, Node.js >= 24 and Playwright Chromium"
    return 0
  fi
  step 5 'Check dependencies' ok "profile $profile, package manager ${pm:-none}"
  check_git
  check_node
  check_chromium
  check_pnpm
  if [ "$windows" = 1 ]; then
    curl_ver="$(probe curl --version)"
    sub ok "curl ${curl_ver:-found} (https:// ready URLs)"
    record curl ok "$curl_ver"
  fi
  [ "$profile" = factory ] || return 0
  check_docker
  check_gh
  detect_agents
  sub skip "agent CLIs: ${agents_found:-none found} (not required on the host; they run in the sandbox image)"
  record 'agent CLIs' 'not required on host' "${agents_found:-none}"
  note "Postgres and LiteLLM are services: run the deploy stack (deploy/docker-compose.yml, see deploy/README.md) and set DATABASE_URL"
}

summary() {
  say ''
  say "${c_bold}Summary${c_reset}"
  if [ "$dry_run" = 1 ]; then
    say "  dry run: nothing was downloaded or installed"
    say "  would install: $install_dir/$exe"
  else
    say "  installed: $install_dir/$exe"
    say "  version:   $guard_version"
    if [ "$from_source" = 1 ]; then
      say "  built from source (unverified checksum: n/a)"
    else
      say "  checksum:  sha256 verified"
    fi
  fi
  if [ -n "$dep_rows" ]; then
    say ''
    say "  Dependencies (profile $profile):"
    printf '%s' "$dep_rows"
  fi
  if [ -n "$skip_rows" ]; then
    say ''
    say "  Skipped; run these yourself when you need them:"
    printf '%s' "$skip_rows"
  fi
  case ":${PATH}:" in
    *":${install_dir}:"*) ;;
    *)
      echo
      echo "$install_dir is not on your PATH. Add it, for example in ~/.profile:"
      echo "  export PATH=\"$install_dir:\$PATH\""
      ;;
  esac
  say ''
  say 'Next steps:'
  say '  cd <your app repo>'
  say '  guard init            # detects your app, writes target.toml and e2e-playwright/'
  say '  guard init --install  # same, then npm install + playwright chromium'
  say '  guard doctor          # checks config, node, npx, git, docker, readiness'
  say '  guard up && guard test'
}

main() {
  parse_args "$@"
  ui_init
  detect_target
  dep_rows='' skip_rows='' guard_version='' built='' pm='' download_error='' plan_detail=''
  apt_tried=0 ri_result=''

  if [ "$version" = "latest" ]; then
    label="the latest release"
  else
    label="v${version}"
  fi
  shown_target="${target:-$os/$arch, from source}"
  [ "$from_source" = 1 ] && shown_target="${target:-$os/$arch}, from source"
  say "${c_bold}guard installer${c_reset}  ·  ${label}  ·  ${shown_target}"
  [ "$dry_run" = 1 ] && say "dry run: printing the plan only"
  say ''

  if [ -n "$target" ]; then
    step 1 'Detect platform' ok "$target"
  else
    step 1 'Detect platform' warn "$os/$arch: no prebuilt binary"
    if [ "$from_source" != 1 ]; then
      if ask_yn "Build guard from source with cargo (rustup is offered if Rust is missing)?" n; then
        from_source=1
      else
        die "no prebuilt binary for $os/$arch; rerun with --from-source to build it with cargo (the installer offers the official rustup when Rust is missing)"
      fi
    fi
  fi

  if [ "$dry_run" != 1 ]; then
    if [ -z "${GUARD_ASSET_FILE:-}" ] || [ "$from_source" = 1 ]; then
      command -v curl >/dev/null 2>&1 || die "curl is required"
    fi
    [ "$from_source" = 1 ] || command -v tar >/dev/null 2>&1 || die "tar is required"
  fi

  work="$(mktemp -d)"
  # A trap that returns lets sh carry on after the signal: exit explicitly
  # (the EXIT trap then removes $work).
  trap 'rm -rf "$work"' EXIT
  trap 'rm -rf "$work"; exit 130' INT
  trap 'rm -rf "$work"; exit 143' TERM
  pkg_manager

  choose_profile

  if [ "$from_source" != 1 ]; then
    asset="guard-${target}.tar.gz"
    if [ -n "${GUARD_ASSET_FILE:-}" ]; then
      s3='Use local archive'
      use_local_asset
    else
      s3='Download and verify'
      if ! download_release; then
        step 3 "$s3" fail "$download_error"
        if ask_yn "no prebuilt release found; build from source?" n; then
          from_source=1
        else
          die "$download_error; rerun with --from-source to build it with cargo"
        fi
      fi
    fi
    if [ "$from_source" != 1 ]; then
      if [ "$dry_run" = 1 ]; then
        step 3 "$s3" plan "$asset"
        sub plan "$plan_detail"
        sub plan "verify sha256, then extract $exe"
      else
        verify_and_extract
        step 3 "$s3" ok "$asset, sha256 verified"
      fi
    fi
  fi
  if [ "$from_source" = 1 ]; then
    build_from_source
  fi

  install_binary
  deps_for_profile
  summary
}

main "$@"
