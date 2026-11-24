#!/usr/bin/env python3
"""Insert an `hdr` node into the first output block for NAME.

The mode comes from $HDR_MODE (default "auto"): with "auto" only fullscreen HDR
content switches the output to PQ/BT.2020, so the desktop stays SDR and keeps
its normal colours. "on" forces the whole session into the HDR blend space,
which makes SDR desktop content look different on most monitors.

niri uses the first matching `output` block, so appending a second block is
ignored. This helper searches the main config and its `.kdl` includes, inserts
the hdr node inside the matching block when missing, and appends a block only
when none exists.

Usage: [HDR_MODE=auto|on] hdr-config.py <output-name> <config-dir>
"""

import glob
import os
import pathlib
import re
import sys

name, root = sys.argv[1], pathlib.Path(sys.argv[2])
mode = os.environ.get("HDR_MODE", "auto").strip().lower()
if mode not in ("auto", "on"):
    raise SystemExit(f"unsupported HDR_MODE={mode!r} (use auto or on)")
block = f'    hdr mode="{mode}" {{\n        reference-luminance 203\n    }}\n'
files = [root / "config.kdl"] + sorted(glob.glob(str(root / "**/*.kdl"), recursive=True))
seen = set()
for file in files:
    file = pathlib.Path(file)
    if file in seen:
        continue
    seen.add(file)
    try:
        lines = file.read_text().splitlines(keepends=True)
    except OSError:
        continue
    for i, line in enumerate(lines):
        if not re.match(rf'\s*output\s+"{re.escape(name)}"\s*\{{', line):
            continue
        depth = line.count("{") - line.count("}")
        j = i + 1
        has_hdr = False
        while j < len(lines) and depth > 0:
            depth += lines[j].count("{") - lines[j].count("}")
            if re.search(r"\bhdr\b", lines[j]):
                has_hdr = True
            j += 1
        if has_hdr:
            print(f"hdr already present in {file}")
            sys.exit(0)
        lines.insert(i + 1, block)
        file.write_text("".join(lines))
        print(f"added hdr mode={mode} for {name} inside {file}")
        sys.exit(0)

with (root / "config.kdl").open("a") as fh:
    fh.write(f'\noutput "{name}" {{\n{block}}}\n')
print(f"no existing output block for {name}; appended one with hdr mode={mode}")
