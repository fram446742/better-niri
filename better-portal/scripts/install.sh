#!/usr/bin/env bash
# Install or uninstall better-portal without cargo-xtask or root by default.
#
# Default (--user): install into $HOME/.local, the user D-Bus service directory,
# the user systemd unit directory, and the user xdg-desktop-portal config.
#
# Every destination is overridable through flags or environment variables. Use
# --stage DIR or DESTDIR to render a package tree without touching system or
# home directories. --dry-run prints every action and writes nothing.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(cd -- "$script_dir/.." && pwd)

program=better-portal
picker=better-portal-picker
bus_name=org.freedesktop.impl.portal.desktop.better

mode=install
dry_run=0
system=0
build=1
binaries=1
write_portals_conf=1
print_paths=0

stage_arg=
prefix_arg=
destdir_arg=
bindir_arg=
datadir_arg=
portals_dir_arg=
dbus_services_dir_arg=
systemd_user_dir_arg=
portals_conf_arg=
binary_dir_arg=

usage() {
    cat <<'EOF'
Usage: scripts/install.sh [OPTIONS]

Install better-portal, its picker, the D-Bus activation file, the optional
systemd user unit, the .portal descriptor, and a portals.conf preference.

Options:
  --user                 User-local install (default; prefix ~/.local).
  --system               System-wide install (prefix /usr/local, config /etc/xdg).
  --prefix DIR           Install prefix. Default: $HOME/.local or /usr/local.
  --destdir DIR          Staging root prepended to every install destination.
  --bindir DIR           Binary directory. Default: $PREFIX/bin.
  --datadir DIR          Shared-data directory. Default: $PREFIX/share.
  --portals-dir DIR      xdg-desktop-portal descriptor directory.
                         Default: $DATADIR/xdg-desktop-portal/portals.
  --dbus-services-dir DIR
                         Session D-Bus service directory.
                         Default: $DATADIR/dbus-1/services.
  --systemd-user-dir DIR systemd user unit directory.
                         Default: $XDG_CONFIG_HOME/systemd/user (user) or
                         $PREFIX/lib/systemd/user (system).
  --portals-conf FILE    Preference file. Default:
                         $XDG_CONFIG_HOME/xdg-desktop-portal/portals.conf (user)
                         or /etc/xdg/xdg-desktop-portal/portals.conf (system).
  --binary-dir DIR       Directory containing the release binaries.
                         Default: $CARGO_TARGET_DIR/release or ./target/release.
  --no-build             Do not run cargo build --release.
  --no-binaries          Do not install better-portal or better-portal-picker.
  --no-portals-conf      Do not create or update portals.conf.
  --stage DIR            Shorthand for --destdir DIR --no-build --no-binaries:
                         render the configuration artifacts into DIR only.
  --uninstall            Remove the installed files. With a merged
                         portals.conf, restore the .better-portal.bak backup.
  --dry-run              Print planned actions and write nothing.
  --print-paths          Print resolved KEY=VALUE paths and exit.
  -h, --help             Show this help.

Environment overrides (flags win):
  PREFIX, DESTDIR, BINDIR, DATADIR, PORTALS_DIR, DBUS_SERVICES_DIR,
  SYSTEMD_USER_DIR, PORTALS_CONF, XDG_CONFIG_HOME, CARGO_TARGET_DIR,
  BETTER_PORTAL_BINDIR, BETTER_PORTAL_DATADIR, BETTER_PORTAL_PORTALS_DIR,
  BETTER_PORTAL_DBUS_SERVICES_DIR, BETTER_PORTAL_SYSTEMD_USER_DIR,
  BETTER_PORTAL_PORTALS_CONF, BETTER_PORTAL_BINARY_DIR
EOF
}

die() {
    printf 'better-portal install: %s\n' "$*" >&2
    exit 1
}

need_value() {
    [ "$#" -ge 2 ] || die "$1 needs a value"
}

while [ "$#" -gt 0 ]; do
    case $1 in
        --user)
            system=0
            shift
            ;;
        --system)
            system=1
            shift
            ;;
        --prefix)
            need_value "$@"
            prefix_arg=$2
            shift 2
            ;;
        --destdir)
            need_value "$@"
            destdir_arg=$2
            shift 2
            ;;
        --bindir)
            need_value "$@"
            bindir_arg=$2
            shift 2
            ;;
        --datadir)
            need_value "$@"
            datadir_arg=$2
            shift 2
            ;;
        --portals-dir)
            need_value "$@"
            portals_dir_arg=$2
            shift 2
            ;;
        --dbus-services-dir)
            need_value "$@"
            dbus_services_dir_arg=$2
            shift 2
            ;;
        --systemd-user-dir)
            need_value "$@"
            systemd_user_dir_arg=$2
            shift 2
            ;;
        --portals-conf)
            need_value "$@"
            portals_conf_arg=$2
            shift 2
            ;;
        --binary-dir)
            need_value "$@"
            binary_dir_arg=$2
            shift 2
            ;;
        --no-build)
            build=0
            shift
            ;;
        --no-binaries)
            binaries=0
            shift
            ;;
        --no-portals-conf)
            write_portals_conf=0
            shift
            ;;
        --stage)
            need_value "$@"
            stage_arg=$2
            shift 2
            ;;
        --uninstall)
            mode=uninstall
            shift
            ;;
        --dry-run)
            dry_run=1
            shift
            ;;
        --print-paths)
            print_paths=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1 (try --help)"
            ;;
    esac
done

PREFIX=${prefix_arg:-${PREFIX:-}}
if [ "$system" = 1 ] && [ -z "$PREFIX" ]; then
    PREFIX=/usr/local
fi
PREFIX=${PREFIX:-$HOME/.local}
PREFIX=${PREFIX%/}

DESTDIR=${destdir_arg:-${DESTDIR:-}}
if [ -n "$DESTDIR" ]; then
    DESTDIR=${DESTDIR%/}
fi

if [ -n "$stage_arg" ]; then
    DESTDIR=${stage_arg%/}
    build=0
    binaries=0
fi

BINDIR=${bindir_arg:-${BETTER_PORTAL_BINDIR:-${BINDIR:-$PREFIX/bin}}}
DATADIR=${datadir_arg:-${BETTER_PORTAL_DATADIR:-${DATADIR:-$PREFIX/share}}}
PORTALS_DIR=${portals_dir_arg:-${BETTER_PORTAL_PORTALS_DIR:-${PORTALS_DIR:-$DATADIR/xdg-desktop-portal/portals}}}
DBUS_SERVICES_DIR=${dbus_services_dir_arg:-${BETTER_PORTAL_DBUS_SERVICES_DIR:-${DBUS_SERVICES_DIR:-$DATADIR/dbus-1/services}}}

config_home=${XDG_CONFIG_HOME:-$HOME/.config}
if [ "$system" = 1 ]; then
    default_systemd_dir=$PREFIX/lib/systemd/user
    default_portals_conf=/etc/xdg/xdg-desktop-portal/portals.conf
else
    default_systemd_dir=$config_home/systemd/user
    default_portals_conf=$config_home/xdg-desktop-portal/portals.conf
fi

SYSTEMD_USER_DIR=${systemd_user_dir_arg:-${BETTER_PORTAL_SYSTEMD_USER_DIR:-${SYSTEMD_USER_DIR:-$default_systemd_dir}}}
PORTALS_CONF=${portals_conf_arg:-${BETTER_PORTAL_PORTALS_CONF:-${PORTALS_CONF:-$default_portals_conf}}}

BINARY_DIR=${binary_dir_arg:-${BETTER_PORTAL_BINARY_DIR:-${CARGO_TARGET_DIR:-$root/target}/release}}

PORTAL_BIN=$BINDIR/$program
PICKER_BIN=$BINDIR/$picker
PORTAL_FILE=$PORTALS_DIR/better.portal
DBUS_SERVICE=$DBUS_SERVICES_DIR/$bus_name.service
SYSTEMD_UNIT=$SYSTEMD_USER_DIR/better-portal.service

stage_path() {
    printf '%s%s' "$DESTDIR" "$1"
}

print_resolved_paths() {
    printf 'MODE=%s\n' "$mode"
    printf 'USER_INSTALL=%s\n' "$([ "$system" = 0 ] && echo 1 || echo 0)"
    printf 'PREFIX=%s\n' "$PREFIX"
    printf 'DESTDIR=%s\n' "$DESTDIR"
    printf 'BINDIR=%s\n' "$BINDIR"
    printf 'DATADIR=%s\n' "$DATADIR"
    printf 'PORTALS_DIR=%s\n' "$PORTALS_DIR"
    printf 'DBUS_SERVICES_DIR=%s\n' "$DBUS_SERVICES_DIR"
    printf 'SYSTEMD_USER_DIR=%s\n' "$SYSTEMD_USER_DIR"
    printf 'PORTALS_CONF=%s\n' "$PORTALS_CONF"
    printf 'BINARY_DIR=%s\n' "$BINARY_DIR"
    printf 'PORTAL_BIN=%s\n' "$PORTAL_BIN"
    printf 'PICKER_BIN=%s\n' "$PICKER_BIN"
    printf 'PORTAL_FILE=%s\n' "$PORTAL_FILE"
    printf 'DBUS_SERVICE=%s\n' "$DBUS_SERVICE"
    printf 'SYSTEMD_UNIT=%s\n' "$SYSTEMD_UNIT"
}

if [ "$print_paths" = 1 ]; then
    print_resolved_paths
    exit 0
fi

install_file() {
    local src=$1 dst=$2 file_mode=$3
    local staged
    staged=$(stage_path "$dst")
    if [ "$dry_run" = 1 ]; then
        printf 'install -Dm%s %s %s\n' "$file_mode" "$src" "$staged"
    else
        install -Dm"$file_mode" "$src" "$staged"
    fi
}

render_template() {
    local src=$1 dst=$2 file_mode=$3
    local staged
    staged=$(stage_path "$dst")
    if [ "$dry_run" = 1 ]; then
        printf 'render %s -> %s (Exec=%s)\n' "$src" "$staged" "$PORTAL_BIN"
    else
        mkdir -p -- "$(dirname -- "$staged")"
        sed -e "s|@BETTER_PORTAL@|$PORTAL_BIN|g" "$src" > "$staged.better-portal.tmp"
        chmod "$file_mode" "$staged.better-portal.tmp"
        mv -f -- "$staged.better-portal.tmp" "$staged"
    fi
}

remove_file() {
    local dst=$1 staged
    staged=$(stage_path "$dst")
    if [ "$dry_run" = 1 ]; then
        printf 'rm -f %s\n' "$staged"
    else
        rm -f -- "$staged"
    fi
}

merge_portals_conf() {
    local src=$1 dst=$2 staged backup new
    staged=$(stage_path "$dst")
    backup=$staged.better-portal.bak

    if [ ! -f "$staged" ]; then
        install_file "$src" "$dst" 644
        return
    fi

    if grep -q '^# Installed by better-portal' "$staged"; then
        install_file "$src" "$dst" 644
        return
    fi

    if [ "$dry_run" = 1 ]; then
        printf 'merge %s into %s (backup %s)\n' "$src" "$staged" "$backup"
        return
    fi

    mkdir -p -- "$(dirname -- "$staged")"
    [ -f "$backup" ] || cp -p -- "$staged" "$backup"
    new=$staged.better-portal.new

    awk '
        BEGIN {
            screenshot = "org.freedesktop.impl.portal.Screenshot=better;"
            screencast = "org.freedesktop.impl.portal.ScreenCast=better;"
            in_preferred = 0
            seen_preferred = 0
            seen_screenshot = 0
            seen_screencast = 0
        }
        function emit_missing() {
            if (!seen_screenshot) {
                print screenshot
                seen_screenshot = 1
            }
            if (!seen_screencast) {
                print screencast
                seen_screencast = 1
            }
        }
        /^[[:space:]]*\[preferred\][[:space:]]*$/ {
            print
            in_preferred = 1
            seen_preferred = 1
            next
        }
        /^[[:space:]]*\[/ {
            if (in_preferred) {
                emit_missing()
                in_preferred = 0
            }
            print
            next
        }
        in_preferred && /^[[:space:]]*org\.freedesktop\.impl\.portal\.Screenshot[[:space:]]*=/ {
            print screenshot
            seen_screenshot = 1
            next
        }
        in_preferred && /^[[:space:]]*org\.freedesktop\.impl\.portal\.ScreenCast[[:space:]]*=/ {
            print screencast
            seen_screencast = 1
            next
        }
        { print }
        END {
            if (!seen_preferred) {
                print "[preferred]"
                emit_missing()
            } else if (in_preferred) {
                emit_missing()
            }
        }
    ' "$staged" > "$new"

    if ! grep -q '^# Installed by better-portal' "$new"; then
        {
            printf '# Installed by better-portal; scripts/install.sh --uninstall restores %s\n' "$(basename -- "$backup")"
            cat "$new"
        } > "$new.marked"
        mv -f -- "$new.marked" "$new"
    fi
    chmod 644 "$new"
    mv -f -- "$new" "$staged"
}

uninstall_portals_conf() {
    local staged backup
    staged=$(stage_path "$PORTALS_CONF")
    backup=$staged.better-portal.bak
    if [ -f "$backup" ]; then
        if [ "$dry_run" = 1 ]; then
            printf 'restore %s -> %s\n' "$backup" "$staged"
        else
            mv -f -- "$backup" "$staged"
        fi
    elif [ -f "$staged" ] && grep -q '^# Installed by better-portal' "$staged"; then
        remove_file "$PORTALS_CONF"
    fi
}

if [ "$mode" = uninstall ]; then
    if [ "$binaries" = 1 ]; then
        remove_file "$PORTAL_BIN"
        remove_file "$PICKER_BIN"
    fi
    remove_file "$PORTAL_FILE"
    remove_file "$DBUS_SERVICE"
    remove_file "$SYSTEMD_UNIT"
    if [ "$write_portals_conf" = 1 ]; then
        uninstall_portals_conf
    fi
    if [ "$dry_run" != 1 ]; then
        printf 'better-portal uninstalled from %s\n' "$([ -n "$DESTDIR" ] && printf '%s' "$DESTDIR" || printf '%s' "$PREFIX")"
    fi
    exit 0
fi

if [ "$build" = 1 ] && [ "$binaries" = 1 ]; then
    if [ "$dry_run" = 1 ]; then
        printf 'cargo build --release --manifest-path %s/Cargo.toml\n' "$root"
    else
        cargo build --release --manifest-path "$root/Cargo.toml"
    fi
fi

if [ "$binaries" = 1 ] && [ "$dry_run" != 1 ]; then
    [ -x "$BINARY_DIR/$program" ] || die "missing $BINARY_DIR/$program; run without --no-build or pass --binary-dir"
    [ -x "$BINARY_DIR/$picker" ] || die "missing $BINARY_DIR/$picker; run without --no-build or pass --binary-dir"
fi

if [ "$binaries" = 1 ]; then
    install_file "$BINARY_DIR/$program" "$PORTAL_BIN" 755
    install_file "$BINARY_DIR/$picker" "$PICKER_BIN" 755
fi

install_file "$root/better.portal" "$PORTAL_FILE" 644
render_template "$root/packaging/org.freedesktop.impl.portal.desktop.better.service" "$DBUS_SERVICE" 644
render_template "$root/packaging/better-portal.service" "$SYSTEMD_UNIT" 644

if [ "$write_portals_conf" = 1 ]; then
    merge_portals_conf "$root/packaging/portals.conf" "$PORTALS_CONF"
fi

if [ "$dry_run" = 1 ]; then
    printf '\nDry run: wrote nothing.\n'
    exit 0
fi

printf '\nInstalled better-portal:\n'
printf '  daemon:  %s\n' "$(stage_path "$PORTAL_BIN")"
printf '  picker:  %s\n' "$(stage_path "$PICKER_BIN")"
printf '  portal:  %s\n' "$(stage_path "$PORTAL_FILE")"
printf '  dbus:    %s\n' "$(stage_path "$DBUS_SERVICE")"
printf '  systemd: %s\n' "$(stage_path "$SYSTEMD_UNIT")"
if [ "$write_portals_conf" = 1 ]; then
    printf '  config:  %s\n' "$(stage_path "$PORTALS_CONF")"
fi
printf '\nNext steps:\n'
printf '  systemctl --user daemon-reload   # if you installed the systemd unit\n'
printf '  restart xdg-desktop-portal, or log out and back in\n'
