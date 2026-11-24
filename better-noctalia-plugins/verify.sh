#!/usr/bin/env bash
# Validate the Noctalia plugin source without running the shell.
#
# - parses every plugin.toml and checks the required identity fields;
# - checks that every [[widget]] entry points at an existing file;
# - parses every .luau entry with lua5.4 (syntax only);
# - checks that plugin ids are unique and match their directory name;
# - checks translation JSON files.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd "$root"

command -v python3 >/dev/null || { echo "python3 is required"; exit 1; }
lua_bin=${LUA_BIN:-}
if [ -z "$lua_bin" ]; then
    for candidate in luau lua5.4 lua5.3 lua; do
        if command -v "$candidate" >/dev/null 2>&1; then
            lua_bin=$candidate
            break
        fi
    done
fi
[ -n "$lua_bin" ] || { echo "no Lua interpreter found (set LUA_BIN)"; exit 1; }

python3 - "$root" <<'PY'
import json
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
ids = {}
count = 0
for manifest in sorted(root.glob("*/plugin.toml")):
    data = tomllib.loads(manifest.read_text())
    plugin_dir = manifest.parent.name
    for key in ("id", "name", "version", "plugin_api"):
        if key not in data:
            raise SystemExit(f"{manifest}: missing {key}")
    if "/" not in data["id"]:
        raise SystemExit(f"{manifest}: id must be author/plugin")
    if data["id"].split("/", 1)[1] != plugin_dir:
        raise SystemExit(
            f"{manifest}: id suffix must match directory {plugin_dir!r}"
        )
    if data["id"] in ids:
        raise SystemExit(f"duplicate plugin id {data['id']}")
    ids[data["id"]] = str(manifest)
    widgets = data.get("widget", [])
    if not widgets:
        raise SystemExit(f"{manifest}: no [[widget]] entries")
    for widget in widgets:
        entry = widget.get("entry")
        if not entry:
            raise SystemExit(f"{manifest}: widget without entry")
        path = manifest.parent / entry
        if not path.is_file():
            raise SystemExit(f"{manifest}: missing entry {path}")
        count += 1
    translations = manifest.parent / "translations"
    if translations.is_dir():
        for lang in translations.glob("*.json"):
            json.loads(lang.read_text())
print(f"manifests: {len(ids)} plugins, {count} widget entries")
PY

while IFS= read -r -d '' script; do
    SCRIPT="$script" "$lua_bin" -e 'assert(loadfile(os.getenv("SCRIPT")))'
    echo "ok: ${script#"$root"/}"
done < <(find "$root" -name '*.luau' -print0 | sort -z)

echo "all Noctalia plugin checks passed (syntax: $lua_bin)"
