# Shared helpers for the packaging scripts.
#
# This file is sourced; it does not enable `set -e` or change the caller's
# options. It needs python3 >= 3.11 (tomllib) and cargo on PATH.

die() {
    printf 'packaging: %s\n' "$*" >&2
    exit 1
}

repo_root() {
    local dir
    dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
    printf '%s\n' "$dir"
}

# Print a scalar field from a profile, or one array item per line.
profile_field() {
    local file=$1 key=$2
    python3 - "$file" "$key" <<'PY'
import sys, tomllib

path, key = sys.argv[1], sys.argv[2]
with open(path, "rb") as fh:
    data = tomllib.load(fh)
if key not in data:
    sys.exit(f"profile {path}: missing key {key!r}")
value = data[key]
if isinstance(value, list):
    for item in value:
        if not isinstance(item, str):
            sys.exit(f"profile {path}: {key!r} must contain strings")
        print(item)
elif isinstance(value, str):
    print(value)
else:
    sys.exit(f"profile {path}: {key!r} must be a string or an array of strings")
PY
}

profile_name() {
    profile_field "$1" name
}

profile_description() {
    profile_field "$1" description
}

profile_features() {
    profile_field "$1" cargo_features
}

profile_apps() {
    profile_field "$1" apps
}

# Map a profile component name to its Cargo manifest, relative to the repo root.
# An empty result means the root niri manifest.
component_manifest() {
    case $1 in
        niri) printf '%s\n' "" ;;
        portal) printf '%s\n' better-portal/Cargo.toml ;;
        companion) printf '%s\n' better-companion/Cargo.toml ;;
        monitor) printf '%s\n' better-monitor/Cargo.toml ;;
        session) printf '%s\n' better-session/Cargo.toml ;;
        shot) printf '%s\n' better-shot/Cargo.toml ;;
        wallpaper) printf '%s\n' better-wallpaper/Cargo.toml ;;
        settings) printf '%s\n' better-settings/Cargo.toml ;;
        audio) printf '%s\n' better-audio/Cargo.toml ;;
        sidepanels) printf '%s\n' better-sidepanels/Cargo.toml ;;
        *) return 1 ;;
    esac
}

# Print the binaries a component is expected to produce, one per line.
component_binaries() {
    case $1 in
        niri) printf '%s\n' niri ;;
        portal) printf '%s\n' better-portal better-portal-picker ;;
        companion) printf '%s\n' better-companion companionctl ;;
        monitor) printf '%s\n' better-monitor ;;
        session) printf '%s\n' better-session ;;
        shot) printf '%s\n' better-shot ;;
        wallpaper) printf '%s\n' better-wallpaper ;;
        settings) printf '%s\n' better-settings better-settingsctl ;;
        audio) printf '%s\n' better-audio ;;
        sidepanels) printf '%s\n' better-sidepanels ;;
        *) return 1 ;;
    esac
}

all_components() {
    printf '%s\n' \
        niri portal companion monitor session shot wallpaper settings audio sidepanels
}

# Print the feature names declared by the niri package.
niri_feature_names() {
    local root=$1
    (cd "$root" && cargo metadata --no-deps --format-version 1) |
        python3 -c '
import json, sys
for package in json.load(sys.stdin)["packages"]:
    if package["name"] == "niri":
        for name in sorted(package["features"]):
            print(name)
        break
else:
    sys.exit("cargo metadata did not contain the niri package")
'
}

# Print the binary target names declared by one manifest.
manifest_binary_names() {
    local manifest=$1
    cargo metadata --no-deps --format-version 1 --manifest-path "$manifest" |
        python3 -c '
import json, sys
for package in json.load(sys.stdin)["packages"]:
    for target in package["targets"]:
        if "bin" in target["kind"]:
            print(target["name"])
'
}

# Resolve a component's manifest to an absolute path.
component_manifest_path() {
    local root=$1 component=$2 rel
    rel=$(component_manifest "$component") || return 1
    if [ -z "$rel" ]; then
        printf '%s\n' "$root/Cargo.toml"
    else
        printf '%s\n' "$root/$rel"
    fi
}

# Assert that every requested feature exists on the niri package.
validate_features() {
    local root=$1 profile=$2
    shift 2
    local available feature
    available=$(niri_feature_names "$root") || die "cannot read niri features with cargo metadata"
    for feature in "$@"; do
        if ! printf '%s\n' "$available" | grep -qxF -- "$feature"; then
            die "profile $profile names unknown Cargo feature '$feature' (available: $(printf '%s' "$available" | paste -sd' ' -))"
        fi
    done
}

# Assert that every component name is known and has a manifest.
validate_apps() {
    local root=$1 profile=$2
    shift 2
    local app manifest
    for app in "$@"; do
        manifest=$(component_manifest_path "$root" "$app") ||
            die "profile $profile names unknown component '$app' (known: $(all_components | paste -sd' ' -))"
        [ -f "$manifest" ] || die "profile $profile component '$app' has no manifest at $manifest"
    done
}
