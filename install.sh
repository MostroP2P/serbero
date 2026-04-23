#!/bin/sh
# install.sh — Serbero installer
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/MostroP2P/serbero/main/install.sh | sh
#
# Environment variables:
#   SERBERO_INSTALL_DIR  Directory to install the binary into. Overrides the
#                        default /usr/local/bin or ~/.local/bin selection.
#   NO_COLOR             Disable ANSI color output (any non-empty value).
#
# Dependencies: curl or wget, grep, sed, uname, chmod, mkdir. All POSIX.
# Checksum verification is optional: if neither `sha256sum` nor `shasum` is
# available, the script warns and continues without verifying.

set -e

REPO='MostroP2P/serbero'
BINARY_BASENAME='serbero'
API_URL="https://api.github.com/repos/${REPO}/releases/latest"
RELEASES_URL="https://github.com/${REPO}/releases"

# --- Color / formatting ----------------------------------------------------
# Only emit ANSI codes when stdout is a terminal and NO_COLOR is not set.
# `[ -t 1 ]` is POSIX and works on dash, ash, and bash.
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    C_RESET=$(printf '\033[0m')
    C_GREEN=$(printf '\033[0;32m')
    C_RED=$(printf '\033[0;31m')
    C_BLUE=$(printf '\033[0;34m')
    C_YELLOW=$(printf '\033[0;33m')
else
    C_RESET=''
    C_GREEN=''
    C_RED=''
    C_BLUE=''
    C_YELLOW=''
fi

info() { printf '%s→%s %s\n' "$C_BLUE" "$C_RESET" "$1"; }
ok()   { printf '%s✓%s %s\n' "$C_GREEN" "$C_RESET" "$1"; }
warn() { printf '%s!%s %s\n' "$C_YELLOW" "$C_RESET" "$1" >&2; }
err()  { printf '%s✗%s %s\n' "$C_RED" "$C_RESET" "$1" >&2; }

have() { command -v "$1" >/dev/null 2>&1; }

# --- Detect OS and architecture --------------------------------------------
detect_target() {
    os_raw=$(uname -s)
    arch_raw=$(uname -m)
    case "$os_raw" in
        Linux)  os=linux ;;
        Darwin) os=macos ;;
        *)
            err "Unsupported OS: $os_raw"
            err "See $RELEASES_URL for all available binaries."
            exit 1
            ;;
    esac
    case "$arch_raw" in
        x86_64|amd64)   arch=x86_64 ;;
        aarch64|arm64)  arch=arm64 ;;
        *)
            err "Unsupported architecture: $arch_raw"
            err "See $RELEASES_URL for all available binaries."
            exit 1
            ;;
    esac
    RELEASE_ASSET="${BINARY_BASENAME}-${os}-${arch}"
    PLATFORM_LABEL="${os}/${arch}"
}

# --- HTTP wrappers ---------------------------------------------------------
# Download URL $1 → local path $2. Progress indicator when stdout is a TTY.
http_download() {
    url=$1
    out=$2
    if have curl; then
        if [ -t 1 ]; then
            curl -fL --progress-bar -o "$out" "$url"
        else
            curl -fsSL -o "$out" "$url"
        fi
    elif have wget; then
        if [ -t 1 ]; then
            wget --show-progress -qO "$out" "$url"
        else
            wget -qO "$out" "$url"
        fi
    else
        err "Neither curl nor wget found; one is required."
        exit 1
    fi
}

# Fetch URL $1 and emit body + a trailing "HTTP_STATUS:NNN" marker line so
# the caller can distinguish rate-limiting (403) from network errors.
http_fetch_with_status() {
    url=$1
    if have curl; then
        curl -sS -w '\nHTTP_STATUS:%{http_code}' "$url" || printf '\nHTTP_STATUS:000'
    elif have wget; then
        # wget doesn't expose the HTTP status cleanly on success, so we
        # synthesize one. It exits non-zero on any HTTP error, which we
        # flatten to 000 (treated as "network error" by the caller).
        if body=$(wget -qO - "$url" 2>/dev/null); then
            printf '%s\nHTTP_STATUS:200' "$body"
        else
            printf 'HTTP_STATUS:000'
        fi
    else
        err "Neither curl nor wget found; one is required."
        exit 1
    fi
}

# --- Fetch latest release tag ----------------------------------------------
fetch_latest_tag() {
    info "Fetching latest release tag from GitHub API..."
    response=$(http_fetch_with_status "$API_URL")
    status=$(printf '%s' "$response" | sed -n 's/^HTTP_STATUS:\([0-9]\{1,\}\)$/\1/p' | tail -n1)
    body=$(printf '%s' "$response" | sed '/^HTTP_STATUS:[0-9]\{1,\}$/d')
    case "$status" in
        200) ;;
        403)
            err "GitHub API rate limit reached. Download manually from $RELEASES_URL"
            exit 1
            ;;
        000|'')
            err "Failed to reach GitHub API (network error)."
            err "Check your internet connection, or download manually from $RELEASES_URL"
            exit 1
            ;;
        *)
            err "GitHub API returned HTTP $status."
            err "Download manually from $RELEASES_URL"
            exit 1
            ;;
    esac
    # Parse "tag_name": "vX.Y.Z" from the JSON response without jq.
    tag=$(printf '%s' "$body" \
        | grep '"tag_name"' \
        | head -n 1 \
        | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')
    if [ -z "$tag" ]; then
        err "Could not parse tag_name from GitHub API response."
        err "Download manually from $RELEASES_URL"
        exit 1
    fi
    LATEST_TAG=$tag
}

# --- Existing-install detection -------------------------------------------
# Note: we deliberately DO NOT execute the existing binary (e.g.
# `$existing --version`) to read its version. An installer running as
# root would otherwise invoke untrusted pre-existing code during a
# status probe — the threat model includes a previously-installed
# binary that may have been tampered with. Reporting "Replaced
# previous installation" based on file presence is less informative
# than a precise version string, but it is safe unconditionally.
detect_previous_install() {
    existing=$1
    PREV_INSTALLED=0
    if [ -e "$existing" ]; then
        PREV_INSTALLED=1
    fi
}

# --- Pick install directory ------------------------------------------------
# Priority:
#   1. $SERBERO_INSTALL_DIR (operator override)
#   2. /usr/local/bin  if running as root OR already writable by this user
#   3. ~/.local/bin    otherwise (no sudo needed)
#
# Prompting for sudo from a `curl | sh` invocation is fragile (stdin is the
# pipe), so we deliberately do not attempt it. Users who need a system-wide
# install can re-run as root or set SERBERO_INSTALL_DIR.
choose_install_dir() {
    if [ -n "${SERBERO_INSTALL_DIR:-}" ]; then
        INSTALL_DIR=$SERBERO_INSTALL_DIR
        INSTALL_REASON='SERBERO_INSTALL_DIR override'
        return
    fi
    if [ "$(id -u)" = 0 ]; then
        warn "Running as root — installing to /usr/local/bin."
        INSTALL_DIR=/usr/local/bin
        INSTALL_REASON='running as root'
        return
    fi
    if [ -w /usr/local/bin ]; then
        INSTALL_DIR=/usr/local/bin
        INSTALL_REASON='/usr/local/bin is writable'
        return
    fi
    INSTALL_DIR="${HOME}/.local/bin"
    INSTALL_REASON='user-local fallback (no root required)'
}

# --- Checksum verification -------------------------------------------------
# `sha256sum -c --ignore-missing` is deliberately NOT used: if the
# release's checksums.sha256 happened to be malformed or missing our
# specific asset, that flag would silently return success with zero
# entries actually verified. Instead, we extract exactly the line for
# our asset into a single-entry checksum file and run `sha256sum -c`
# (no --ignore-missing), so a missing or mis-formatted entry becomes
# a loud failure.
verify_checksum() {
    dir=$1
    asset=$2
    # Anchored match: start-of-line hex hash, whitespace, optional
    # `*` binary-mode marker, asset name, end-of-line. Guards against
    # substring collisions (e.g. `serbero-linux-x86_64` inside
    # `serbero-linux-x86_64.sig` if signatures are added later).
    expected=$(grep -E "^[0-9a-fA-F]+[[:space:]]+[*]?${asset}\$" \
        "${dir}/checksums.sha256" | head -n 1)
    if [ -z "$expected" ]; then
        err "checksums.sha256 does not contain an entry for ${asset}."
        err "The release appears malformed. Download manually from $RELEASES_URL"
        exit 1
    fi
    printf '%s\n' "$expected" > "${dir}/checksums.single"

    if have sha256sum; then
        (cd "$dir" && sha256sum -c checksums.single >/dev/null) \
            || { err "Checksum verification failed."; exit 1; }
    elif have shasum; then
        (cd "$dir" && shasum -a 256 -c checksums.single >/dev/null) \
            || { err "Checksum verification failed."; exit 1; }
    else
        warn 'Neither sha256sum nor shasum found; skipping checksum verification.'
        return
    fi
    rm -f "${dir}/checksums.single"
    ok 'Checksum verified.'
}

# --- Main ------------------------------------------------------------------
main() {
    detect_target
    info "Detected platform: $PLATFORM_LABEL (asset: $RELEASE_ASSET)"

    fetch_latest_tag
    info "Latest release: $LATEST_TAG"

    choose_install_dir
    info "Install directory: $INSTALL_DIR ($INSTALL_REASON)"
    mkdir -p "$INSTALL_DIR"

    dest="${INSTALL_DIR}/${BINARY_BASENAME}"
    detect_previous_install "$dest"

    # Place the tmpdir INSIDE the install dir so the final mv is a
    # same-filesystem rename and stays atomic. If /tmp is a separate
    # tmpfs (common on many Linux distros), a tmpdir in /tmp would
    # make mv fall back to copy+unlink — losing atomicity and
    # leaving a window where another process could see a partial
    # binary. Using the install-dir parent guarantees rename(2)
    # semantics. PID suffix avoids collisions without depending on
    # mktemp template behaviour that varies between GNU and BSD.
    tmpdir="${INSTALL_DIR}/.serbero-install.$$"
    rm -rf "$tmpdir"
    if ! mkdir -p "$tmpdir"; then
        err "Failed to create temp dir at $tmpdir"
        err "The install directory must be writable for atomic installs."
        exit 1
    fi
    trap 'rm -rf "$tmpdir"' EXIT

    asset_url="https://github.com/${REPO}/releases/download/${LATEST_TAG}/${RELEASE_ASSET}"
    checksums_url="https://github.com/${REPO}/releases/download/${LATEST_TAG}/checksums.sha256"

    info "Downloading $RELEASE_ASSET..."
    http_download "$asset_url" "${tmpdir}/${RELEASE_ASSET}"

    # Download checksums only if we have a tool that can verify them, to
    # avoid a useless round-trip.
    if have sha256sum || have shasum; then
        info 'Downloading checksums.sha256...'
        http_download "$checksums_url" "${tmpdir}/checksums.sha256"
        info 'Verifying checksum...'
        verify_checksum "$tmpdir" "$RELEASE_ASSET"
    else
        warn 'Skipping checksum verification (install a sha256 tool for provenance checks).'
    fi

    chmod +x "${tmpdir}/${RELEASE_ASSET}"
    # Same-filesystem rename → atomic: no other shell process can
    # ever see a partial binary at $dest.
    if ! mv "${tmpdir}/${RELEASE_ASSET}" "$dest" 2>/dev/null; then
        err "Cannot write $dest (permission denied)."
        if [ "$INSTALL_DIR" = '/usr/local/bin' ]; then
            err 'Re-run as root, or set SERBERO_INSTALL_DIR to a writable directory.'
        fi
        exit 1
    fi

    # Sanity-check the resulting file without executing it. We do
    # NOT run `$dest --version`: the installer deliberately avoids
    # invoking downloaded code (see detect_previous_install).
    if [ ! -x "$dest" ]; then
        err "Installed binary at $dest is not executable."
        exit 1
    fi

    if [ "$PREV_INSTALLED" = 1 ]; then
        ok "Replaced previous installation — Serbero $LATEST_TAG now at $dest"
    else
        ok "Serbero $LATEST_TAG installed to $dest"
    fi

    # PATH diagnostic — a common source of "command not found" confusion
    # when the fallback directory is used.
    case ":${PATH:-}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            warn "$INSTALL_DIR is not on your PATH."
            warn 'Add this to your shell profile (~/.bashrc, ~/.zshrc, ~/.profile):'
            warn "    export PATH=\"$INSTALL_DIR:\$PATH\""
            ;;
    esac

    printf '\n'
    printf 'Next steps:\n'
    printf '  1. Fetch the sample config (the binary ships alone — the sample\n'
    printf '     lives in the repo and must be downloaded separately):\n'
    printf '       curl -fsSL https://raw.githubusercontent.com/%s/main/config.sample.toml -o config.toml\n' "$REPO"
    printf '  2. Edit config.toml with your keys, relays, and solvers\n'
    printf '  3. Run: serbero\n'
    printf '\n'
    printf 'Documentation: https://github.com/%s\n' "$REPO"
}

main "$@"
