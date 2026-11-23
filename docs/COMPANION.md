# better-companion architecture

better-companion is the window/automation daemon of the ecosystem. It absorbs
nsticky's workflow and Piri's automation ideas. This document decides how it is
modularised.

## 1. Decision: one daemon, internal modules

better-companion is **one process with internal modules**, not one daemon per
feature.

Reasons:

- One niri IPC connection and one event stream. Multiple daemons would duplicate
  state, reconnection, initial synchronisation and polling.
- One state manager avoids races between sticky, staging, rules and hooks.
- Deterministic rule ordering: module priorities are evaluated in one place.
- Lower memory and fewer wakeups, which matters on laptops.
- One service to package, start, restart and diagnose.
- nsticky's code can be absorbed without running two sticky engines.

A module is therefore a Rust module (later optionally a crate) compiled into the
daemon, with its own config, state and event subscriptions. It is not a `.so`
plugin and not a second process.

## 2. When a separate process/repository is justified

Split only when at least one is true:

- independent lifecycle or release cadence;
- a security boundary (portal permissions, screen capture, audio);
- heavy dependencies or a GUI;
- the component is useful without the compositor/companion;
- it needs to survive companion restarts independently.

Current splits: better-portal, better-shot, better-monitor, better-session,
better-audio, better-wallpaper, better-settings. better-companion stays focused
on windows, workspaces and automation.

## 3. Module layout

Start with internal modules; extract crates only when the boundary is stable.

```
better-companion/
  src/
    main.rs              daemon
    bin/companionctl.rs  CLI
    core/
      niri.rs            single IPC client + reconnect
      state.rs           windows, workspaces, outputs, focus
      events.rs          typed event bus
      config.rs          TOML + hot reload + validation
      api.rs             versioned UDS API
      modules.rs         registry, ordering, lifecycle
    modules/
      sticky/            sticky primitive + fallback
      stage/             staged windows
      scratchpad/        scratchpad stacks
      rules/             window/workspace rules
      run_or_raise/
      singleton/
      swallow/
      hooks/             session/monitor/workspace hooks
      idle/              candidate (Stasis-like)
      rotate/            candidate (iio-niri-like)
```

If compile times or tests justify it, `core` and the larger modules become
workspace crates (`companion-core`, `companion-sticky`, ...). Do not start with
a dozen crates and a plugin framework.

## 4. Module contract

Every module implements the same internal lifecycle:

```rust
trait Module {
    fn name(&self) -> &'static str;
    fn priority(&self) -> i32;
    fn init(&mut self, ctx: &mut ModuleContext) -> Result<()>;
    fn on_event(&mut self, event: &Event, ctx: &mut ModuleContext) -> Result<()>;
    fn on_config(&mut self, config: &Config, ctx: &mut ModuleContext) -> Result<()>;
    fn shutdown(&mut self, ctx: &mut ModuleContext);
}
```

Rules:

- Modules never open their own niri IPC connection; they use the core client.
- Modules receive filtered events and enqueue actions; they do not block.
- State changes go through the core state manager.
- Configuration lives under `[modules.<name>]`, with validation and defaults.
- Ordering is explicit and deterministic; conflicts are resolved by priority and
  rule specificity, and explained by `companionctl explain`.
- A module can be disabled at runtime and must then do no work and hold no
  timers.
- Errors in one module are reported and do not kill the daemon.
- Modules that can execute commands or inject input require explicit
  permissions/allowlists; dry-run is available.
- No module may require another companion service to start, only optional
  adapters.

## 4.1 Action categories and ownership

Modules are organised by **action category**. A category is a stable namespace
with one owner module; the core dispatcher routes by prefix and rejects
duplicates at startup.

### Companion action categories

| Category / prefix | Owner module | Actions (examples) | State |
| --- | --- | --- | --- |
| `sticky.*` | `sticky` | add, remove, toggle, list, set follow policy | pinned windows, per-output policy |
| `stage.*` | `stage` | add, remove, toggle, list, restore, add-all, remove-all | staged windows, stage workspace |
| `scratchpad.*` | `scratchpad` | add, restore, toggle, list, set workspace | LIFO stacks, parking areas |
| `rules.*` | `rules` | reload, validate, explain, dry-run, set priority | rule set |
| `run-or-raise.*` | `run-or-raise` | focus-or-spawn, cycle, list matches | app match cache |
| `singleton.*` | `singleton` | ensure, focus-existing, spawn-policy | instance tracking |
| `swallow.*` | `swallow` | enable, disable, associate, status | parent/child map |
| `workspace-hooks.*` | `hooks` | on-enter, on-leave, run, list | hook table |
| `idle.*` | `idle` (candidate) | inhibit, release, set profile, status | idle state |
| `rotate.*` | `rotate` (candidate) | enable, disable, set orientation, status | sensor state |

Core, not module-owned:

| Category / prefix | Responsibility |
| --- | --- |
| `core.state.*` | query windows, workspaces, outputs, focus |
| `core.events.*` | subscribe, unsubscribe, replay initial state |
| `core.config.*` | load, reload, validate, current path |
| `core.modules.*` | list, enable, disable, status |
| `core.api.*` | version, capabilities, permissions |
| `core.diagnostics.*` | status, dump, logs, explain routing |

### Rules for ownership

1. One action prefix has exactly one owner. Registering a duplicate is a startup
   error, not a silent override.
2. Modules may **query** other modules through the core API, but may not take
   over their actions.
3. Cross-module workflows call the owner: e.g. `stage.restore` asks `sticky` to
   pin a restored window; it does not reimplement pinning.
4. `rules` may **invoke** actions from other categories (that is its purpose),
   but it does not own them. Rule evaluation is deterministic and explainable.
5. Events are separate from actions. Modules subscribe to typed events
   (`window.opened`, `window.closed`, `workspace.activated`, `output.changed`,
   `focus.changed`, `config.reloaded`), and only receive what they subscribe to.
6. Every action returns a typed result/error, is listed by
   `companionctl actions --json`, and is covered by the module's tests.

### Naming

- CLI: `companionctl sticky toggle`, `companionctl stage restore`,
  `companionctl scratchpad toggle`, `companionctl rules explain`.
- UDS/JSON: `sticky.toggle`, `stage.restore`, `scratchpad.toggle`.
- Config: `[modules.sticky]`, `[modules.stage]`, `[modules.scratchpad]`.
- Rust: `modules::sticky::StickyModule`, one `Action` enum per category.

### Dispatch

```
CLI / UDS / rule engine
        │
        ▼
  core action registry ── prefix → owner module
        │
        ▼
  module handler (validates, mutates its state, calls core/native actions)
        │
        ▼
  core state manager + one niri IPC client
```

The registry is built at startup from the enabled modules. Disabled modules do
not register handlers and receive no events. The core never grows a match arm
per feature.

### Third-party actions

Third parties do not load `.so` modules. They can:
- call the documented UDS API;
- run as an external service and expose their own actions;
- ask better-companion to invoke them through a configured, permissioned hook.

If a third-party action category becomes broadly useful, it can be absorbed as a
compiled module.

## 5. Shared core vs modules

Core owns:

- niri IPC client, reconnect, initial state reconciliation;
- state manager (windows, workspaces, outputs, focus, casts);
- event bus and subscriptions;
- config load/validate/hot reload;
- module registry, ordering, diagnostics;
- CLI and UDS API;
- permissions and dry-run.

Modules own:

- sticky/stage/scratchpad workflow (from nsticky);
- window/workspace rules, run-or-raise, singleton, swallow (from Piri);
- session/monitor/workspace hooks (adapters to other services);
- optional idle/rotate helpers.

## 6. nsticky absorption

- The compositor owns the native sticky primitive.
- `modules/sticky`: uses the native primitive when the capability is present,
  otherwise keeps nsticky's move-based fallback.
- `modules/stage`: staging and the stage workspace.
- `modules/scratchpad`: LIFO scratchpads, parking areas, selector.
- `modules/rules`: nsticky's app/title/output/floating rules, merged with Piri's
  rule engine (one rule model, not two).
- Compatibility: accept `nsticky` CLI commands and its config path as aliases in
  better-companion. Do not modify or deprecate the standalone nsticky
  repository; users who want only sticky/staged can keep using it.
- Preserve history/attribution when merging code (BSD-3-Clause).

## 7. Configuration and permissions

```toml
[modules.sticky]
enabled = true

[modules.scratchpad]
enabled = true
workspace = "scratch"

[modules.rules]
enabled = true
dry_run = false
allow_spawn = false

[permissions]
execute = ["^/usr/bin/(foot|firefox)$"]
input = false
```

- TOML for companion config (niri stays KDL).
- Hot reload and validation; invalid config keeps the previous one.
- No arbitrary shell execution by default.
- Input injection is off by default and requires explicit permission.
- Every external request is validated; sockets are `0600`.

## 8. Piri reuse

- Fork Piri preserving history, authorship and licence.
- Keep its event subscription, state sync, reconnection and rule ideas.
- Flatten its extension model into compiled modules unless the audit shows a
  compelling reason to keep its plugin API.
- Do not run two sticky systems: Piri's sticky and nsticky's sticky merge into
  one `sticky` module with nsticky's semantics as the base.

## 9. Roadmap

### Phase A — foundation (first delivery)

- Core IPC/state/events/config/CLI/reconnect.
- `sticky`, `stage`, `scratchpad` from nsticky, with compatibility aliases.
- Capability detection and graceful fallback on plain Niri.

### Phase B — automation

- Unified `rules` (nsticky + Piri), `run_or_raise`, `singleton`, `swallow`.
- `companionctl explain` and dry-run.

### Phase C — hooks and ecosystem adapters

- Workspace/output/focus hooks.
- Optional adapters to better-session, better-monitor, better-audio,
  better-wallpaper; no hard dependency.

### Phase D — optional helpers

- idle/rotate modules or separate services, decided by scope.

## 10. Open questions

1. Internal Rust modules first, or workspace crates from day one?
   (Recommendation: internal modules; extract when stable.)
2. Keep any of Piri's plugin/extension API for third parties, or expose only the
   versioned UDS API and compiled modules? (Recommendation: UDS + compiled
   modules; no `.so`.)
3. Are idle and auto-rotate better-companion modules or independent services?
   (Recommendation: modules first.)
4. Does better-companion need a GUI, or only CLI + UDS + Noctalia plugins?
   (Recommendation: no GUI; better-settings covers configuration.)
