#!/bin/sh
# sds installer: downloads the latest signed sds release for this machine, verifies its
# Ed25519 signature against the pinned release key and installs it. Without an OpenSSL that
# can verify Ed25519 (stock macOS), it checks the release's SHA256SUMS instead and says so.
#
#   curl -fsSL https://raw.githubusercontent.com/altipard/sleep-docker-sleep/main/install.sh | sh
#
# Environment:
#   SDS_VERSION      release to install instead of the latest (e.g. 0.4.1); same as `sds update --tag`.
#                    Only signed releases (0.4.1 and later) can be installed.
#   SDS_INSTALL_DIR  target directory (default: /usr/local/bin if writable, else ~/.local/bin)
#   NO_COLOR         disable colors and the logo (https://no-color.org)
#
# Strict POSIX sh. Everything runs inside main(), which is called on the last line, so a
# truncated download executes nothing.

set -eu

REPO=altipard/sleep-docker-sleep
RELEASES="https://github.com/$REPO/releases"

# Ed25519 release key (SPKI, base64). Must match RELEASE_PUBLIC_KEY_HEX in src/cli/update.rs;
# it is pinned here instead of downloaded so a compromised release page cannot swap it.
RELEASE_KEY_B64='MCowBQYDK2VwAyEAUpfnMu4T/uGhPdEk3yL2OmY1r6DMBccAGFpdyXDsVW8='
# Signature of the string `sds-release-key-check` (same vector as the test in update.rs). Used to
# find an OpenSSL that can really verify Ed25519 before anything is downloaded.
SELFTEST_SIG_B64='onFCnpf5kQkLoF7+vltET3pXr6XSamvVrQLgzpCtwAzQa231AdqQZca8d0KTfBxaN5DB3OCOhZAk8TMcBNVqDw=='

tmp=''
stage=''

cleanup() {
    if [ -n "$stage" ]; then rm -f "$stage"; fi
    if [ -n "$tmp" ]; then rm -rf "$tmp"; fi
    return 0
}

# --- terminal handling ------------------------------------------------------------------

# Sets: ESC, BOLD, DIM, ACC, RST (empty without color), COLOR (0/1), COLOR_MODE, ART (0/1).
# Color only if stdout is a terminal (fd 1, not stdin: `curl | sh` pipes stdin), NO_COLOR is
# empty and TERM is set and not dumb.
init_ui() {
    ESC=$(printf '\033')
    BOLD=''; DIM=''; ACC=''; RST=''
    COLOR=0; COLOR_MODE=16; ART=0
    if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ -n "${TERM:-}" ] && [ "${TERM:-}" != dumb ]; then
        COLOR=1
        case "${COLORTERM:-}" in
            truecolor | 24bit) COLOR_MODE=truecolor ;;
            *)
                case "$TERM" in
                    *256color*) COLOR_MODE=256 ;;
                esac
                ;;
        esac
        art_color fg L
        BOLD="${ESC}[1m"; DIM="${ESC}[2m"; ACC="${ESC}[${acode}m"; RST="${ESC}[0m"
        # The logo needs UTF-8 half-block glyphs and ~48 columns.
        case "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" in
            *[Uu][Tt][Ff]-8* | *[Uu][Tt][Ff]8*)
                cols=${COLUMNS:-}
                case "$cols" in
                    '' | *[!0-9]*) cols=$(tput cols 2>/dev/null || printf '80') ;;
                esac
                case "$cols" in
                    '' | *[!0-9]*) cols=80 ;;
                esac
                if [ "$cols" -ge 48 ]; then ART=1; fi
                ;;
        esac
    fi
    # Errors go to stderr; color them only if stderr is a terminal too.
    ERR_RED=''; ERR_RST=''
    if [ "$COLOR" = 1 ] && [ -t 2 ]; then ERR_RED="${ESC}[1;31m"; ERR_RST="${ESC}[0m"; fi
}

say() { printf '  %s\n' "$*"; }
blank() { printf '\n'; }

fail() {
    printf '%ssds installer: error:%s %s\n' "$ERR_RED" "$ERR_RST" "$*" >&2
    exit 1
}

# --- logo --------------------------------------------------------------------------------

# Palette taken from assets/logo/sds-logo.svg (nightcap, cuff, whale shades, closed eye, zZz).
# Sets p_rgb (truecolor), p_256 (xterm 256) and p_16 (0-7 normal, 8-15 bright).
art_pal() {
    case $1 in
        P) p_rgb='94;99;182';    p_256=61;  p_16=4 ;;  # nightcap   #5e63b6
        W) p_rgb='245;241;230';  p_256=255; p_16=15 ;; # cuff, pompom #f5f1e6
        B) p_rgb='1;155;198';    p_256=38;  p_16=6 ;;  # whale      #019bc6
        L) p_rgb='35;194;238';   p_256=45;  p_16=14 ;; # tail, waterline #23c2ee
        D) p_rgb='0;130;173';    p_256=31;  p_16=6 ;;  # belly shade #0082ad
        F) p_rgb='191;219;224';  p_256=152; p_16=7 ;;  # chin       #bfdbe0
        E) p_rgb='58;77;84';     p_256=238; p_16=8 ;;  # closed eye #3a4d54
        Z) p_rgb='159;164;232';  p_256=147; p_16=12 ;; # zZz (lighter than the logo: readable on dark terminals)
    esac
}

# art_color fg|bg LETTER -> acode (SGR parameters for the active color mode)
art_color() {
    art_pal "$2"
    case "$COLOR_MODE:$1" in
        truecolor:fg) acode="38;2;$p_rgb" ;;
        truecolor:bg) acode="48;2;$p_rgb" ;;
        256:fg) acode="38;5;$p_256" ;;
        256:bg) acode="48;5;$p_256" ;;
        16:fg) if [ "$p_16" -ge 8 ]; then acode=$((p_16 + 82)); else acode=$((p_16 + 30)); fi ;;
        16:bg) if [ "$p_16" -ge 8 ]; then acode=$((p_16 + 92)); else acode=$((p_16 + 40)); fi ;;
    esac
}

# Two pixel rows per terminal row: ' ' = empty, full and half blocks carry fg/bg.
art_render() {
    while IFS= read -r top && IFS= read -r bot; do
        line='  '
        cur=''
        while [ -n "$top" ]; do
            t=${top%"${top#?}"}
            top=${top#?}
            b=${bot%"${bot#?}"}
            bot=${bot#?}
            glyph=' '
            style=''
            if [ "$t" = . ] && [ "$b" = . ]; then
                :
            elif [ "$t" = "$b" ]; then
                # Full block instead of a colored space: the shape survives copy and paste.
                glyph='█'
                art_color fg "$t"
                style=$acode
                art_color bg "$t"
                style="$style;$acode"
            elif [ "$b" = . ]; then
                glyph='▀'
                art_color fg "$t"
                style=$acode
            elif [ "$t" = . ]; then
                glyph='▄'
                art_color fg "$b"
                style=$acode
            else
                glyph='▀'
                art_color fg "$t"
                style=$acode
                art_color bg "$b"
                style="$style;$acode"
            fi
            if [ "$style" != "$cur" ]; then
                if [ -z "$style" ]; then
                    line="$line${ESC}[0m"
                else
                    line="$line${ESC}[0;${style}m"
                fi
                cur=$style
            fi
            line="$line$glyph"
        done
        printf '%s%s[0m\n' "$line" "$ESC"
    done <<'ART'
............PP..........................ZZZZ
..........PPPPPPPPPPPP.....................Z
.......PPPPPPPPPPPPPPPPP..................Z.
......PPPPPPPPPPPPPPPPPPPP.........ZZZ..ZZZZ
..WW.PPPPPPPPPPPPPPPPPPPPPP..........Z......
.WWWWPPPPPPPPPPPPPPPPPPPPPPP........Z.......
.WWWWPPP.PPPPPPPPPPPPPPPPPPP.......ZZZ......
..WW......PPPPPPPPPPPPPPPPPPP.ZZZ...........
..........PPPPPPPPPPPPPPPPPP....Z....L......
.........PPPPPPPPPPPPPPPPPPP...Z....LLL.....
........PPPPPPPPPPPPPPPPPPPPP.ZZZ..LLLL..LLL
......WWWWWWWWWWWWWWWWWWWWWWWW...LLLLLLLLLLL
......WWWWWWWWWWWWWWWWWWWWWWWW...LLLLLLLLL..
.......LLLLLLLLLLLLLLLLLLLLLLLLLLLLLL.......
......BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBL.......
.......BBBBBBBBBBBBBBBBBBBBBBBBBBBB.........
.......BBBBBEBBEBBBBBBBBBBBBBBBBBB..........
........BBBBBEEBBBBBBBBBBBBBBBBB............
.........FFFFFBBBBBBBBBBBBBBDDD.............
............FFFFBBBBBBBBDDDD................
ART
}

# --- steps -------------------------------------------------------------------------------

usage() {
    cat <<'EOF'
sds installer

Usage: curl -fsSL https://raw.githubusercontent.com/altipard/sleep-docker-sleep/main/install.sh | sh

Environment:
  SDS_VERSION      release to install instead of the latest (e.g. 0.4.1); signed releases only
  SDS_INSTALL_DIR  target directory (default: /usr/local/bin if writable, else ~/.local/bin)
  NO_COLOR         disable colors and the logo
EOF
}

# Sets OS, ARCH (names as used by the release assets: sds-<os>-<arch>).
detect_platform() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux) OS=linux ;;
        Darwin) OS=macos ;;
        *) fail "unsupported operating system: $os (sds supports Linux and macOS)" ;;
    esac
    case "$arch" in
        x86_64 | amd64) ARCH=x86_64 ;;
        aarch64 | arm64) ARCH=aarch64 ;;
        *) fail "unsupported architecture: $arch (releases exist for x86_64 and aarch64)" ;;
    esac
    # A shell under Rosetta reports x86_64 on Apple Silicon; install the native binary.
    if [ "$OS" = macos ] && [ "$ARCH" = x86_64 ]; then
        if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then ARCH=aarch64; fi
    fi
}

fetch() { # url file
    curl -fsS --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 15 --retry 2 -L -o "$2" "$1"
}

write_release_key() { # file
    printf '%s\n%s\n%s\n' '-----BEGIN PUBLIC KEY-----' "$RELEASE_KEY_B64" '-----END PUBLIC KEY-----' >"$1"
}

# Stock macOS ships LibreSSL, which cannot verify raw Ed25519 signatures. Probe every candidate
# with the known test vector instead of guessing from the version string.
find_openssl() {
    write_release_key "$tmp/release.pem"
    printf '%s' 'sds-release-key-check' >"$tmp/check.msg"
    for candidate in openssl /opt/homebrew/bin/openssl /usr/local/bin/openssl \
        /opt/homebrew/opt/openssl@3/bin/openssl /usr/local/opt/openssl@3/bin/openssl; do
        command -v "$candidate" >/dev/null 2>&1 || continue
        printf '%s\n' "$SELFTEST_SIG_B64" | "$candidate" base64 -d -A >"$tmp/check.sig" 2>/dev/null || continue
        if "$candidate" pkeyutl -verify -rawin -pubin -inkey "$tmp/release.pem" \
            -in "$tmp/check.msg" -sigfile "$tmp/check.sig" >/dev/null 2>&1; then
            OPENSSL=$(command -v "$candidate")
            return 0
        fi
    done
    return 1
}

# Sets SHA256_TOOL (sha256sum on Linux, shasum on macOS).
find_sha256() {
    for candidate in sha256sum shasum; do
        if command -v "$candidate" >/dev/null 2>&1; then
            SHA256_TOOL=$candidate
            return 0
        fi
    done
    return 1
}

sha256_of() { # file
    case "$SHA256_TOOL" in
        sha256sum) sum=$(sha256sum "$1") || return 1 ;;
        shasum) sum=$(shasum -a 256 "$1") || return 1 ;;
    esac
    printf '%s\n' "${sum%% *}"
}

resolve_version() {
    if [ -n "${SDS_VERSION:-}" ]; then
        VERSION=${SDS_VERSION#v}
        PINNED=1
    else
        final=$(curl -fsSIL --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 15 \
            -o /dev/null -w '%{url_effective}' "$RELEASES/latest") ||
            fail "cannot reach github.com to look up the latest release"
        VERSION=${final##*/}
        PINNED=0
    fi
    # The tag ends up in a URL: accept only a plain version string.
    case "$VERSION" in
        [0-9]*) ;;
        *) fail "could not determine a release version (got '$VERSION')" ;;
    esac
    case "$VERSION" in
        *[!0-9A-Za-z.+-]*) fail "invalid release version '$VERSION'" ;;
    esac
}

# Sets DEST (directory).
choose_dest() {
    if [ -n "${SDS_INSTALL_DIR:-}" ]; then
        DEST=${SDS_INSTALL_DIR%/}
        case "$DEST" in
            /*) ;;
            *) fail "SDS_INSTALL_DIR must be an absolute path (got '$DEST')" ;;
        esac
    elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
        DEST=/usr/local/bin
    elif [ "$(id -u)" = 0 ]; then
        DEST=/usr/local/bin
    elif [ -n "${HOME:-}" ]; then
        DEST="$HOME/.local/bin"
    else
        fail "no writable install directory; set SDS_INSTALL_DIR"
    fi
}

# Fail before downloading anything if the target is not usable.
prepare_dest() {
    mkdir -p "$DEST" 2>/dev/null || fail "cannot create $DEST; set SDS_INSTALL_DIR to a writable directory or run as root"
    [ -w "$DEST" ] || fail "no write access to $DEST; set SDS_INSTALL_DIR to a writable directory or run as root"
}

verify_signature() {
    fetch "$RELEASES/download/$VERSION/$asset.sig" "$tmp/$asset.sig" ||
        fail "download failed: $RELEASES/download/$VERSION/$asset.sig (refusing an unsigned release)"
    say "Verifying signature..."
    "$OPENSSL" pkeyutl -verify -rawin -pubin -inkey "$tmp/release.pem" \
        -in "$tmp/$asset" -sigfile "$tmp/$asset.sig" >/dev/null 2>&1 ||
        fail "signature verification failed for $asset; the download is corrupt or has been tampered with. Nothing was installed."
}

verify_checksum() {
    fetch "$RELEASES/download/$VERSION/SHA256SUMS" "$tmp/SHA256SUMS" ||
        fail "could not download SHA256SUMS of release $VERSION (releases up to 0.4.1 have none).
  Install OpenSSL 3 (macOS: brew install openssl) to verify the signature instead. Nothing was installed."
    say "Verifying checksum..."
    expected=$(awk -v name="$asset" '$2 == name || $2 == "*" name { print $1 }' "$tmp/SHA256SUMS")
    # Exactly one SHA-256: anything else (missing, duplicate, malformed) is refused.
    case "$expected" in
        '' | *[!0-9a-f]*) fail "no valid checksum for $asset in SHA256SUMS. Nothing was installed." ;;
    esac
    [ "${#expected}" -eq 64 ] || fail "no valid checksum for $asset in SHA256SUMS. Nothing was installed."
    actual=$(sha256_of "$tmp/$asset") || fail "could not compute the checksum of $asset. Nothing was installed."
    [ "$actual" = "$expected" ] ||
        fail "checksum mismatch for $asset; the download is corrupt or has been tampered with. Nothing was installed."
    say "Note: checksum only. The release signature was not checked, that needs OpenSSL 3."
}

install_binary() { # downloaded-file
    stage="$DEST/.sds-install.$$"
    cp "$1" "$stage" || fail "cannot write to $DEST"
    chmod 755 "$stage"
    # Same check as `sds update`: the new binary must run and report the expected version.
    reported=$("$stage" --version 2>&1) ||
        fail "the downloaded binary does not run on this system (Linux releases up to 0.5.0 need glibc 2.38 or newer); nothing was installed"
    [ "$reported" = "sds $VERSION" ] ||
        fail "the downloaded binary reports '$reported', expected 'sds $VERSION'; nothing was installed"
    KEPT=0
    if [ -e "$DEST/sds" ]; then
        cp -p "$DEST/sds" "$DEST/sds.previous" || fail "cannot keep the previous binary at $DEST/sds.previous"
        KEPT=1
    fi
    mv -f "$stage" "$DEST/sds"
    stage=''
}

main() {
    case "${1:-}" in
        -h | --help) usage; exit 0 ;;
        '') ;;
        *) printf 'sds installer: unknown option %s (see --help)\n' "$1" >&2; exit 2 ;;
    esac

    init_ui
    trap cleanup EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM

    command -v curl >/dev/null 2>&1 || fail "curl is required but was not found"
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/sds-install.XXXXXX") || fail "cannot create a temporary directory"

    blank
    if [ "$ART" = 1 ]; then
        art_render
        blank
    fi
    say "${BOLD}sds installer${RST}${DIM} - sleep docker sleep${RST}"
    blank

    detect_platform
    asset="sds-$OS-$ARCH"
    # The mode is fixed here: a failed signature check never falls back to the checksum.
    if find_openssl; then
        VERIFY=signature
    elif find_sha256; then
        VERIFY=checksum
    else
        fail "found neither an OpenSSL with Ed25519 support nor sha256sum/shasum, so the download cannot be verified. Nothing was installed."
    fi
    resolve_version
    choose_dest
    prepare_dest

    if [ "$PINNED" = 1 ]; then
        say "Using ${ACC}$VERSION${RST} for $OS-$ARCH"
    else
        say "Found ${ACC}$VERSION${RST} for $OS-$ARCH"
    fi
    say "Downloading..."
    fetch "$RELEASES/download/$VERSION/$asset" "$tmp/$asset" ||
        fail "download failed: $RELEASES/download/$VERSION/$asset (does release $VERSION exist for $OS-$ARCH?)"
    if [ "$VERIFY" = signature ]; then
        verify_signature
    else
        verify_checksum
    fi

    install_binary "$tmp/$asset"

    blank
    say "${BOLD}sds${RST} ${ACC}$VERSION${RST} installed to ${BOLD}$DEST/sds${RST}"
    if [ "$KEPT" = 1 ]; then say "${DIM}previous binary kept at $DEST/sds.previous${RST}"; fi
    case ":${PATH:-}:" in
        *":$DEST:"*) ;;
        *)
            blank
            say "$DEST is not on your PATH. Add it with:"
            say "  export PATH=\"$DEST:\$PATH\""
            ;;
    esac
    blank
    # `sds update` replaces the binary in place, so it needs write access to $DEST.
    if [ "$(id -u)" = 0 ]; then upd='sudo sds update'; else upd='sds update'; fi
    say "To update later, run: ${BOLD}$upd${RST}"
    blank
}

main "$@"
