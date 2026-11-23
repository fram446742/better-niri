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

Decision: use a Cargo workspace from day one, but group crates by **bounded
context** instead of one crate per tiny module. This keeps compile times and
ownership clear without a dozen near-empty crates.

```
better-companion/
  Cargo.toml                 workspace
  crates/
    companion-core/          IPC client, state, event bus, config, action registry,
                             override pipeline, permissions, UDS API, diagnostics
    companion-parking/       modules: sticky, stage, scratchpad
    companion-automation/    modules: rules, launch, singleton, swallow
    companion-hooks/         module: hooks (workspace/output/session events)
    companion-system/        modules: idle, orientation (candidates)
    companionctl/            CLI
    better-companion/        daemon binary
```

Rules:

- `companion-core` has no dependency on feature crates; feature crates depend on
  core, never the reverse.
- Each crate exposes one or more action categories; the registry still enforces
  one owner per action prefix.
- A module can be moved to its own crate later without changing its action
  names or config.
- Do not create a crate for a feature that has no independent state or tests.

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
| `window.sticky.*` | `sticky` | add, remove, toggle, list, set follow policy | pinned windows, per-output policy |
| `window.stage.*` | `stage` | add, remove, toggle, list, restore, add-all, remove-all | staged windows, stage workspace |
| `window.scratchpad.*` | `scratchpad` | add, restore, toggle, list, set workspace | LIFO stacks, parking areas |
| `automation.rules.*` | `rules` | reload, validate, explain, dry-run, set priority | rule set |
| `automation.launch.*` | `launch` | focus-or-spawn, cycle, list matches | app match cache |
| `automation.singleton.*` | `singleton` | ensure, focus-existing, spawn-policy | instance tracking |
| `automation.swallow.*` | `swallow` | enable, disable, associate, status | parent/child map |
| `event.hooks.*` | `hooks` | workspace/output/session enter/leave, run, list | hook table |
| `system.idle.*` | `idle` | inhibit, release, set profile, status | idle state |
| `system.orientation.*` | `orientation` | enable, disable, set orientation, status | sensor state |

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

### Naming and hierarchy

Actions are hierarchical and explicit:

- `window.sticky.toggle`
- `window.stage.restore`
- `window.scratchpad.toggle`
- `automation.rules.explain`
- `automation.launch.focus-or-spawn`
- `system.idle.inhibit`

Namespaces (`window`, `automation`, `event`, `system`, `core`) are grouping
nodes; a module owns a **leaf subtree**, not the whole namespace. The registry
uses longest-prefix matching and rejects duplicate subtree owners.

- CLI: `companionctl window sticky toggle`,
  `companionctl automation launch focus-or-spawn`,
  `companionctl system idle inhibit`.
- UDS/JSON: `window.sticky.toggle`, `automation.rules.explain`.
- Config: `[modules.sticky]`, `[modules.stage]`, `[modules.scratchpad]`,
  `[modules.rules]`, `[modules.launch]`, `[modules.idle]`. Config names stay
  short; action names stay explicit.
- Events: `event.window.opened`, `event.window.changed`, `event.window.closed`,
  `event.workspace.activated`, `event.output.changed`, `event.focus.changed`,
  `event.config.reloaded`.
- Rust: `modules::sticky::StickyModule`, one `Action` enum per leaf category.

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

## 4.2 Override pipeline (clean, debuggable overrides)

`rules` may override actions, but only through an explicit pipeline. Ownership
and override are compatible:

```
request (CLI/UDS/rule) 
  → resolve owner
  → interceptors (rules, explicit priority)
  → owner handler
  → result
```

An interceptor can:

- **pass** — continue to the owner;
- **block** — deny with a reason (shown to the caller and logged);
- **rewrite** — replace action/arguments once (loop detection + depth limit);
- **before/after** — enqueue additional actions around the owner call.

Rules:

1. Only `rules` (or an explicitly permissioned module) may register
   interceptors. Modules cannot silently override each other.
2. Order is deterministic: explicit priority, then rule specificity, then
   registration order. No hash-map iteration order.
3. Every rewrite/block is recorded. `companionctl explain <action> --args ...`
   prints the chain: matching rules, rewrites, final owner, result.
4. Loop protection: max rewrite depth and `(action, args)` cycle detection.
5. Overrides apply to external requests by default. Internal calls from a module
   can opt in; core safety actions (config, permissions, shutdown) cannot be
   overridden.
6. Dry-run (`rules.dry_run`) evaluates the chain without executing.
7. Tests cover pass, block, rewrite, ordering, loop protection and permissions.

This gives the power of overrides without losing a single owner per action.

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

## 8. Plugin runtimes and extension models

The runtime-neutral plugin manager and the benchmark plan are specified in
[`PLUGINS.md`](PLUGINS.md). The short version: Piri's compiled `Plugin` trait is
the internal contract, not a public ABI; external services use UDS; native ABI
and WASM are staged experiments.


Piri (MIT, `fdfa170`) is **not** a dynamic plugin host. Its plugins are Rust
modules implementing a `Plugin` trait (`src/plugins/mod.rs`), registered through
a macro, with one Niri event listener, event-interest filtering, config hot
reload and IPC command handling. That is exactly the compiled-module model we
want.

### A. Compiled modules (Piri model) — recommended

- The module is a Rust crate linked into the daemon; full type safety and access
  to core state; no ABI; predictable performance.
- Adding a third-party module requires rebuilding/forking the daemon or shipping
  it as a distro patch.
- Best for first-party features and anything on the event hot path.

### B. Keep Piri's plugin API

- Meaningful if we fork Piri: we keep its `Plugin` trait and registry as the
  internal contract, then extend it with action categories, override pipeline,
  permissions and diagnostics.
- Existing Piri plugins are compiled-in; they cannot be installed as binaries.
- Recommended only as the **internal** model, not as a public binary ABI.

### C. External services over UDS — recommended complement

- A separate process exposes versioned JSON actions/events over a Unix socket.
- Language-agnostic, crash-isolated, independently releasable, permissioned.
- Not suitable for per-frame or very high-frequency events; schema/versioning and
  state reconciliation cost real work.
- Best for integrations (audio, monitor, wallpaper, session, third-party tools).

### D. WASM / Lua plugins

- Sandboxed, runtime-loadable, language-flexible.
- Adds a runtime, host API, async/event bridging, debugging and a large security
  surface; WASM is not free and Lua/Luau is already the Noctalia plugin language.
- Reconsider only if there is real third-party demand and a stable host API.

### E. Dynamic `.so` Rust plugins

- Rejected: no stable Rust ABI; modules are tied to the exact compiler, crate
  versions and `State` layout; unsafe and unmaintainable across upstream merges.

**Recommendation:** Piri's compiled `Plugin` model for first-party modules, plus
the versioned UDS API for external services. No WASM/Lua or `.so` in the first
iteration.

## 8.1 Idle and orientation: module or repository?

### As companion modules

- Reuse the single IPC client, state manager, config and permissions.
- No extra daemon, packaging or reconnect logic.
- Coupled to companion's lifecycle; companion must run.
- Idle policy needs session/app events, which companion already has.
- Orientation needs sensor/DBus access; still a small module.

### As separate services

- Independent lifecycle and release; works without companion.
- Better isolation for system permissions (inhibit, sensors).
- But duplicates niri IPC/state or depends on companion anyway; another service
  to package and supervise.

**Decision:** start both as `companion-system` modules (`idle`,
`orientation`). Split into separate services only if one needs independent
system permissions, runs without companion, or grows its own UI/release cycle.
Action names and config stay compatible if split later.

## Implemented so far

`better-companion/` (separate workspace inside this repository; excluded from
niri's Cargo workspace):

- `companion-core`: JSON-line protocol (`list`, `call`, `status`), merged
  action registry with namespaces, UDS/state path helpers and atomic JSON
  state.
- `companion-parking`: named parking lots (`park`, `unpark`, `list`, `lots`,
  `clear`) persisted under `$XDG_STATE_HOME/better-companion/parking.json`.
- `companion-automation`: named sequences of `(namespace, action, params)`
  steps; `automation.run` executes them through the registry.
- `companion-hooks`: event → commands registration and resolution (`fire`
  returns matches; the daemon does not execute commands implicitly).
- `companion-system`: ping/status/paths.
- `companion-niri`: `compositor.*` bridge over the niri IPC socket — perform a
  typed `Action`, and query version/outputs/workspaces/windows/focused window.
  It also consumes niri's event stream into two auto-tracked parking lots
  (`auto:workspaces`, `auto:windows`), so activation/focus/close/minimize/urgency
  events keep a queryable snapshot without polling. `parker.*` consumes those
  lots: `park`/`restore` a window and `park-workspace`/`restore-workspace` a
  group, minimizing/unminimizing through niri and recording snapshots in the
  `parker` lot.
- `companion-daemon` (`better-companion`): Unix socket server, mode 0600.
- `companionctl`: list/status/call client.

Not yet: Phase B `rules`/`run_or_raise`, the optional adapters/companions, and
richer parking policies (e.g. stashing without minimizing).

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

- idle/orientation modules in `companion-system`; split only if justified.

## 10. Open questions

Resolved:

- workspace crates from day one, grouped by bounded context;
- one daemon with internal category modules;
- clean override pipeline with deterministic ordering and explain/dry-run;
- Piri's compiled `Plugin` model as the internal base;
- UDS for external services;
- idle/orientation start as modules.

Remaining:

- one crate per category or grouped crates (current proposal: grouped);
- whether to expose a WASM/Lua extension host later (not in v1);
- GUI: none in companion; better-settings owns configuration.
