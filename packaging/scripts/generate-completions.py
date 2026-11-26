#!/usr/bin/env python3
"""Generate bash, fish and zsh completions for the Loom CLIs.

The companion CLIs parse their arguments by hand, so the completion lists are
declared here instead of being derived from clap. Run this script after adding a
command or option; the generated files are committed under packaging/completions
and installed by the split packages.

Usage: python3 packaging/scripts/generate-completions.py [--check]
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
OUT = ROOT / "packaging" / "completions"

# bin: (commands, options (name, takes_value), nested {group: [subcommands]})
CLIS: dict[str, tuple[list[str], list[tuple[str, bool]], dict[str, list[str]]]] = {
    "loom": (
        [
            "companion",
            "companion-daemon",
            "portal",
            "portal-picker",
            "monitor",
            "session",
            "shot",
            "wallpaper",
            "settings",
            "settingsctl",
            "audio",
            "sidepanels",
            "tui",
        ],
        [("list", False), ("version", False), ("help", False)],
        {},
    ),
    "loom-tui": (
        [],
        [("help", False), ("version", False)],
        {},
    ),
    "loom-companion": (
        [],
        [
            ("socket", True),
            ("once", False),
            ("config", True),
            ("no-track", False),
            ("help", False),
            ("version", False),
        ],
        {},
    ),
    "loomctl": (
        ["list", "describe", "status", "call", "explain", "history", "config", "subscribe"],
        [("socket", True), ("id", True), ("dry-run", False), ("help", False), ("version", False)],
        {
            "config": ["show", "validate", "reload"],
            "history": ["list", "clear"],
            "explain": ["rules", "windows"],
        },
    ),
    "loom-portal": (
        [],
        [("help", False), ("version", False)],
        {},
    ),
    "loom-portal-picker": (
        [],
        [("screencast", False), ("screencast-window", False), ("help", False), ("version", False)],
        {},
    ),
    "loom-monitor": (
        [
            "list",
            "show",
            "save",
            "apply",
            "confirm",
            "cancel",
            "restore",
            "delete",
            "migrate",
            "position",
            "arrange",
            "rules",
            "watch",
        ],
        [
            ("socket", True),
            ("state", True),
            ("rules-file", True),
            ("plan", True),
            ("json", False),
            ("dry-run", False),
            ("once", False),
            ("confirm-timeout", True),
            ("pending", True),
            ("profile", True),
            ("priority", True),
            ("match-name", True),
            ("make", True),
            ("model", True),
            ("serial", True),
            ("output", True),
            ("debounce-ms", True),
            ("poll-ms", True),
            ("cooldown-ms", True),
            ("help", False),
            ("version", False),
        ],
        {"rules": ["list", "add", "remove", "explain", "apply"]},
    ),
    "loom-session": (
        [
            "list",
            "show",
            "capture",
            "save",
            "restore",
            "explain",
            "deactivate",
            "rename",
            "delete",
            "history",
            "snapshot",
            "export",
            "import",
            "template",
        ],
        [
            ("socket", True),
            ("dry-run", False),
            ("explain", False),
            ("wait-ms", True),
            ("poll-ms", True),
            ("retry", True),
            ("force", False),
            ("snapshot-before", False),
            ("launch", True),
            ("hook-pre", True),
            ("hook-activate", True),
            ("hook-post", True),
            ("hook-deactivate", True),
            ("template", True),
            ("set", True),
            ("monitor-profile", True),
            ("monitor-via", True),
            ("monitor-command", True),
            ("audio-profile", True),
            ("audio-config", True),
            ("audio-command", True),
            ("wallpaper-scene", True),
            ("wallpaper-command", True),
            ("companion-rule", True),
            ("companion-call", True),
            ("companion-mark", True),
            ("companion-command", True),
            ("profile-activate", True),
            ("profile-deactivate", True),
            ("file", True),
            ("merge", False),
            ("replace", False),
            ("keep", True),
            ("max-age-days", True),
            ("auto", False),
            ("name", True),
            ("interval-ms", True),
            ("count", True),
            ("help", False),
            ("version", False),
        ],
        {
            "history": ["list", "show", "restore", "prune"],
            "template": ["list", "show", "apply"],
        },
    ),
    "loom-shot": (
        ["ui", "screen", "window", "record", "annotate", "history", "pin", "ocr"],
        [
            ("socket", True),
            ("path", True),
            ("id", True),
            ("no-pointer", False),
            ("pointer", False),
            ("copy", False),
            ("annotate", True),
            ("hdr", True),
            ("format", True),
            ("quality", True),
            ("metadata", False),
            ("dry-run", False),
            ("json", False),
            ("stop", False),
            ("duration", True),
            ("fps", True),
            ("codec", True),
            ("region", True),
            ("window", True),
            ("output", True),
            ("audio", True),
            ("stitch", True),
            ("stitch-path", True),
            ("no-history", False),
            ("ocr", False),
            ("ocr-sidecar", False),
            ("preview", False),
            ("op", True),
            ("annotations", True),
            ("clear-log", False),
            ("history", False),
            ("list-ops", False),
            ("help", False),
            ("version", False),
        ],
        {},
    ),
    "loom-wallpaper": (
        ["scene", "apply", "watch", "status"],
        [("socket", True), ("backend", True), ("dry-run", False), ("session", True), ("help", False), ("version", False)],
        {"scene": ["set", "use", "session", "list", "show", "export", "import", "delete"]},
    ),
    "loom-audio": (
        [
            "sinks",
            "sources",
            "streams",
            "links",
            "volume",
            "mute",
            "rules",
            "default",
            "profile",
            "apply",
            "restore",
            "watch",
            "diagnose",
            "status",
        ],
        [
            ("config", True),
            ("backend", True),
            ("pactl", True),
            ("wpctl", True),
            ("pw-dump", True),
            ("pw-cli", True),
            ("focus-command", True),
            ("json", False),
            ("dry-run", False),
            ("interval-ms", True),
            ("help", False),
            ("version", False),
        ],
        {
            "rules": ["add", "remove"],
            "profile": ["list", "show", "apply", "restore"],
        },
    ),
    "loom-settings": (
        [],
        [("help", False), ("version", False)],
        {},
    ),
    "loom-settingsctl": (
        [
            "get",
            "set",
            "unset",
            "add-bind",
            "remove-bind",
            "add-window-rule",
            "remove-window-rule",
            "add-layer-rule",
            "remove-layer-rule",
            "add-startup",
            "remove-startup",
            "list",
            "validate",
            "export",
            "import",
            "diff",
            "integrations",
            "displays",
            "sessions",
            "wallpaper",
            "audio",
            "sidepanels",
            "companion",
        ],
        [("config", True), ("dry-run", False), ("raw", True), ("help", False), ("version", False)],
        {},
    ),
    "loom-sidepanels": (
        [],
        [
            ("config", True),
            ("profile", True),
            ("layout", True),
            ("check", False),
            ("validate", False),
            ("init", False),
            ("help", False),
            ("version", False),
        ],
        {},
    ),
}


def bash(binary: str, commands, options, nested) -> str:
    opts = " ".join(f"--{name}" for name, _ in options)
    lines = [
        f"# bash completion for {binary} (generated by packaging/scripts/generate-completions.py)",
        f"_{binary.replace('-', '_')}() {{",
        "    local cur prev cmd",
        "    COMPREPLY=()",
        '    cur="${COMP_WORDS[COMP_CWORD]}"',
        '    prev="${COMP_WORDS[COMP_CWORD-1]}"',
        f'    local commands="{" ".join(commands)}"',
        f'    local options="{opts}"',
        "",
        '    if [[ $cur == -* ]]; then',
        '        COMPREPLY=($(compgen -W "$options" -- "$cur"))',
        "        return 0",
        "    fi",
    ]
    if commands:
        lines += [
            "    if [[ $COMP_CWORD -eq 1 ]]; then",
            '        COMPREPLY=($(compgen -W "$commands" -- "$cur"))',
            "        return 0",
            "    fi",
            '    cmd="${COMP_WORDS[1]}"',
        ]
        if nested:
            lines.append('    case "$cmd" in')
            for group, subs in nested.items():
                lines += [
                    f"        {group})",
                    f'            COMPREPLY=($(compgen -W "{" ".join(subs)}" -- "$cur"))',
                    "            ;;",
                ]
            lines += ["    esac"]
        else:
            lines.append("    COMPREPLY=($(compgen -f -- \"$cur\"))")
    else:
        lines.append("    COMPREPLY=($(compgen -f -- \"$cur\"))")
    lines += ["}", f"complete -F _{binary.replace('-', '_')} {binary}", ""]
    return "\n".join(lines)


def fish(binary: str, commands, options, nested) -> str:
    lines = [
        f"# fish completion for {binary} (generated by packaging/scripts/generate-completions.py)",
        f"complete -c {binary} -f",
    ]
    for name, takes_value in options:
        flag = f"complete -c {binary} -l {name}"
        if takes_value:
            flag += " -r"
        lines.append(flag)
    for command in commands:
        lines.append(
            f"complete -c {binary} -n '__fish_use_subcommand' -a '{command}'"
        )
    for group, subs in nested.items():
        for sub in subs:
            lines.append(
                f"complete -c {binary} -n '__fish_seen_subcommand_from {group}' -a '{sub}'"
            )
    lines.append("")
    return "\n".join(lines)


def zsh(binary: str, commands, options, nested) -> str:
    func = f"_{binary.replace('-', '_')}"
    lines = [
        f"#compdef {binary}",
        f"# zsh completion for {binary} (generated by packaging/scripts/generate-completions.py)",
        f"{func}() {{",
    ]
    if commands:
        lines += [
            "    local -a commands",
            "    commands=(",
        ]
        for command in commands:
            lines.append(f"        '{command}'")
        lines += [
            "    )",
            "    if (( CURRENT == 2 )); then",
            "        _describe 'command' commands",
            "    else",
            "        _files",
            "    fi",
            "}",
            f'{func} "$@"',
        ]
    else:
        lines += [
            "    _files",
            "}",
            f'{func} "$@"',
        ]
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    check = "--check" in sys.argv[1:]
    rendered: dict[pathlib.Path, str] = {}
    for binary, (commands, options, nested) in CLIS.items():
        rendered[OUT / "bash" / binary] = bash(binary, commands, options, nested)
        rendered[OUT / "fish" / f"{binary}.fish"] = fish(binary, commands, options, nested)
        rendered[OUT / "zsh" / f"_{binary}"] = zsh(binary, commands, options, nested)

    stale = []
    for path, content in rendered.items():
        if check:
            if not path.exists() or path.read_text() != content:
                stale.append(path)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
    if check:
        if stale:
            for path in stale:
                print(f"stale: {path.relative_to(ROOT)}", file=sys.stderr)
            return 1
        print(f"completions up to date ({len(rendered)} files)")
        return 0
    print(f"wrote {len(rendered)} completion files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
