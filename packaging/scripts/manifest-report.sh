#!/usr/bin/env bash
# Print the packaging matrix derived from the Cargo manifests.
#
# The output is Markdown so docs/PACKAGING.md can be refreshed from the real
# manifests instead of hand-maintained version lists. Run it from anywhere:
#
#   packaging/scripts/manifest-report.sh > /tmp/matrix.md
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$script_dir/lib.sh"

root=$(repo_root)
cd "$root"

printf '### Manifest pins\n\n'
printf '| Item | Value |\n| --- | --- |\n'
printf '| niri package version | `%s` |\n' \
    "$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="niri"))')"
printf '| Rust MSRV | `%s` |\n' \
    "$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["rust_version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="niri"))')"
printf '| better-smithay revision | `%s` |\n' \
    "$(sed -n '/\[workspace.dependencies.smithay\]/,/^$/p' Cargo.toml | sed -n 's/^rev = "\(.*\)"/\1/p' | head -n1)"
printf '| Default Cargo features | `%s` |\n\n' \
    "$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(", ".join(next(p["features"]["default"] for p in json.load(sys.stdin)["packages"] if p["name"]=="niri")))')"

printf '### Profiles\n\n'
printf '| Profile | Features | Components |\n| --- | --- | --- |\n'
for file in packaging/profiles/*.toml; do
    name=$(profile_name "$file")
    features=$(profile_features "$file" | paste -sd',' - | sed 's/,/, /g')
    apps=$(profile_apps "$file" | paste -sd',' - | sed 's/,/, /g')
    printf '| `%s` | %s | %s |\n' "$name" "${features:-none}" "$apps"
done
printf '\n'

printf '### Components and binaries\n\n'
printf '| Component | Manifest | Binaries |\n| --- | --- | --- |\n'
for component in $(all_components); do
    manifest=$(component_manifest "$component")
    [ -n "$manifest" ] || manifest=Cargo.toml
    bins=$(component_binaries "$component" | paste -sd',' - | sed 's/,/, /g')
    printf '| `%s` | `%s` | %s |\n' "$component" "$manifest" "$bins"
done
printf '\n'

printf '### niri Cargo features\n\n'
printf '| Feature | In default |\n| --- | --- |\n'
default_list=$(cargo metadata --no-deps --format-version 1 |
    python3 -c 'import json,sys; print(" ".join(next(p["features"]["default"] for p in json.load(sys.stdin)["packages"] if p["name"]=="niri")))')
while IFS= read -r feature; do
    [ "$feature" = default ] && continue
    if printf '%s\n' "$default_list" | tr ' ' '\n' | grep -qxF -- "$feature"; then
        printf '| `%s` | yes |\n' "$feature"
    else
        printf '| `%s` | no |\n' "$feature"
    fi
done < <(niri_feature_names "$root")
