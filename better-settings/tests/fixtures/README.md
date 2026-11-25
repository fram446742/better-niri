# Integration fixtures

JSON captured from the companion CLIs and used by the parser tests in
`src/integrations.rs`. They are checked in so the tests need neither the apps
installed nor a compositor.

| Fixture | Produced by |
| --- | --- |
| `monitor-list.json`, `monitor-show.json` | `better-monitor --json list`, `... --json show desk` against a temporary profile store |
| `monitor-apply*.json`, `monitor-restore-dry-run.json` | the `ApplyReport` shape printed by `better-monitor --json apply/restore` |
| `session-list.json`, `session-show.json` | `better-session list`, `... show work` against a temporary context store |
| `session-explain.json`, `session-restore.json` | the `RestoreReport` shape printed by `better-session explain/restore` |
| `wallpaper-status.json`, `wallpaper-scene-list.json`, `wallpaper-use.json` | `better-wallpaper status`, `scene list`, `scene use` against a temporary scene store |
| `wallpaper-apply*.json` | the native/command dry-run and command apply JSON |
| `audio-*.json` | the JSON printed by `better-audio status/sinks/rules/default/apply` |
| `sidepanels-config.json` | a v2 `better-sidepanels/config.json`, accepted by `--validate` |
| `companion-*.json` | the tagged `Response` JSON from `companionctl describe/status/list`, `history`, `explain rules`, and `call idle/rotate/marks` |
| `niri-version.json`, `niri-capabilities.json`, `niri-outputs.json` | `niri msg --json version/capabilities/outputs` |
| `session-history.json` | `better-session history list` |

When a tool's output changes, update the fixture and the corresponding parser in
`src/integrations.rs` together. Fields the settings frontend does not display are
kept so the fixture still looks like real output.
