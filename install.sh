#!/bin/sh
set -eu

fail() {
    printf 'prtui: %s\n' "$*" >&2
    exit 1
}

main() {
    version=latest
    directory="$HOME/.local/bin"
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --head) version=head; shift ;;
            --version|--install-dir)
                [ "$#" -ge 2 ] && [ -n "$2" ] || fail "$1 requires a value"
                case "$1" in
                    --version) version=$2 ;;
                    --install-dir) directory=$2 ;;
                esac
                shift 2
                ;;
            -h|--help)
                printf 'Usage: install.sh [--head | --version VERSION] [--install-dir DIR]\n'
                return
                ;;
            *) fail "unknown option: $1" ;;
        esac
    done
    command -v curl >/dev/null 2>&1 || fail 'curl is required'
    command -v tar >/dev/null 2>&1 || fail 'tar is required'

    case "$(uname -s)" in
        Darwin) platform=apple-darwin ;;
        Linux) platform=unknown-linux-musl ;;
        *) fail 'supported systems are macOS and Linux' ;;
    esac
    case "$(uname -m)" in
        arm64|aarch64) architecture=aarch64 ;;
        x86_64|amd64) architecture=x86_64 ;;
        *) fail 'supported architectures are ARM64 and x86_64' ;;
    esac

    if command -v sha256sum >/dev/null 2>&1; then
        checksum=sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        checksum='shasum -a 256'
    else
        fail 'sha256sum or shasum is required'
    fi

    repository=https://github.com/tale/prtui
    if [ "$version" = latest ]; then
        latest=$(curl -fsSL -o /dev/null -w '%{url_effective}' "$repository/releases/latest")
        version=${latest##*/}
    fi
    case "$version" in
        head) tag=head ;;
        *) tag="v${version#v}" ;;
    esac
    version=${tag#v}
    case "$version" in
        ''|*[!a-zA-Z0-9.+-]*) fail "invalid release version: $version" ;;
    esac

    temporary=$(mktemp -d)
    staging=
    trap 'rm -rf "$temporary"; if [ -n "$staging" ]; then rm -f "$staging"; fi' EXIT
    trap 'exit 1' HUP INT TERM

    name="prtui-$tag-$architecture-$platform"
    url="$repository/releases/download/$tag/$name.tar.gz"
    printf 'Downloading prtui %s for %s-%s\n' "$version" "$architecture" "$platform"
    curl -fsSL "$url" -o "$temporary/$name.tar.gz"
    curl -fsSL "$url.sha256" -o "$temporary/$name.tar.gz.sha256"
    (cd "$temporary" && $checksum -c "$name.tar.gz.sha256")
    tar xzf "$temporary/$name.tar.gz" -C "$temporary" "$name/prtui"

    mkdir -p "$directory"
    [ ! -d "$directory/prtui" ] || fail "$directory/prtui is a directory"
    staging=$(mktemp "$directory/.prtui.XXXXXX")
    install -m 755 "$temporary/$name/prtui" "$staging"
    mv -f "$staging" "$directory/prtui"
    staging=
    printf 'Installed prtui to %s/prtui\n' "$directory"
    case ":$PATH:" in
        *":$directory:"*) ;;
        *) printf 'Add %s to your PATH.\n' "$directory" ;;
    esac
}

main "$@"
