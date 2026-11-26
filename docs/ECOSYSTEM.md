# Loom ecosystem plan

This document connects loom-niri with the wider Loom family. It
turns the reference-project list into concrete responsibilities, interfaces and
phases. It complements `MODULES.md` (what lives inside the compositor) and the
umbrella `EXPANSION_PLAN.m`.

## 1. Principle

**Independent applications, shared protocols, optional integrations, one
coherent ecosystem.**

- The compositor owns privileged, per-frame and correctness-critical behaviour.
- Companions own policy, automation, UI and long-running workflows.
- Every component is useful on its own; missing components degrade gracefully.
- No monolith, no plugin ABI inside the compositor, no duplicate sources of
  truth.

## 2. Layers

### Layer A — Desktop foundation

loom-niri + loom-smithay + upstream Niri compatibility + Wayland/DRM.

Owns: presentation, HDR/colour, input, layout/window primitives, output
management, capture protocols, sticky primitive, virtual outputs, IPC/config.

### Layer B — Native services

loom-nsticky, loom-companion, loom-session, loom-sidepanels, loom-shot,
loom-monitor, loom-audio, loom-wallpaper, loom-settings, optionally
loom-portal.

Own policy, state and UI. Communicate over niri IPC, Unix sockets, D-Bus and
PipeWire. Never in the compositor's render loop.

### Layer C — Interfaces

CLIs, JSON/UDS APIs, Noctalia plugins, user scripts. A user must be able to
operate the desktop without the shell.

## 3. Reference projects and decisions

| Area | References | Decision | Where it lives |
| --- | --- | --- | --- |
| HDR / colour | dividebysandwich, spicy, MEKCCK | Best shared lineage; keep MEKCCK base + losnoco Vulkan/DRM additions | Core (`presentation`) |
| HDR capture / tone mapping | kakxem, spicy | Integrate into capture paths | Core |
| Vulkan | kode54 / losnoco | Optional renderer | Module `vulkan` |
| Direct scan-out | spicy / upstream | Keep and harden | Core |
| Explicit sync, tearing, FIFO, async flips, pointer warp | urayde | Integrate | Core |
| Shaders (global/region/window/focus-ring) | biri | One engine, scopes + presets | Module `effects` |
| Liquid glass | Niri-glass / niri-elite | Effect/preset inside `effects`, not a second engine | Module `effects` |
| Virtual outputs | biri | Output backend primitive | Core (`outputs`, runtime-created) |
| Isolated outputs | biri | Output policy flag | Core (`outputs`) |
| Hidden workspaces | biri | Workspace state | Core (`layout`) |
| Portrait layout | biri | Layout axis | Core (`layout`) |
| Per-keyboard config | biri | Input | Core (`input`) |
| Grid overview | SHORiN / niri-elite | Navigation UI | Module `navigation` |
| Minimisation | SHORiN / niri-elite | Window state | Core (`layout`) |
| Magnifier | SHORiN / niri-elite | Navigation aid | Module `navigation` |
| Hot corners | niri-elite | Input/gesture policy | Core |
| Agent seat | SHORiN / niri-elite | Automation primitive, security-sensitive | Module `agent`, default off |
| KDE protocols / blur | upstream / Naxdy | Optional integration | Module `kde` |
| SVG cursors | niri-elite | Cosmetic dependency | Module `cursor` |
| Shake-to-enlarge | niri-elite | Cursor effect | Module `cursor` |
| Software EGL / multi-GPU | niri-elite | Backend support | Core |
| Native portal | niri-elite | Separate daemon from the start | `loom-portal` |
| Window groups | Naxdy / upstream tabs | Tile-group model with tab indicator | Core (`layout`) |
| Sticky windows | loom-nsticky, biri | Native primitive in core; workflow absorbed into loom-companion | Core primitive + loom-companion |
| Focus-ring shaders | biri | Scope of `effects` | Module `effects` |
| Scratchpads | Piri, niri-scratchpad-rs, nsticky | Companion feature | loom-companion / nsticky |
| Run-or-raise, singleton, swallow | Piri, niri-ror | Companion automation | loom-companion |
| Side panels | niri-sidepanels | Reuse/adapt | loom-sidepanels |
| Sessions | nirinit, niri-session-manager, niri-companion | Combine into contexts | loom-session |
| Config GUI | NiriMod, nirimon, nirilayout | Reference; preserve KDL/comments | loom-settings / loom-monitor |
| Audio routing | PWSW | Audit first (LLM-generated, author warning) | loom-audio |
| Keyboard layout per device | kunai | Our per-keyboard config covers most | Core (`input`); kunai as reference |
| Auto-rotate | iio-niri | Optional service | loom-companion integration |
| Idle/suspend | Stasis | Optional service | loom-companion or separate `loom-idle` |
| Screenshots/recording | niri-shot, nirisnap, utsushot | Compare and reuse | loom-shot |
| Shell | Noctalia, DMS, Exo, iNiR, Ironbar, Glimpse | Shell frozen | Noctalia plugins only |

## 4. Repositories and responsibilities

| Repository | Responsibility | Status |
| --- | --- | --- |
| loom-niri | Compositor, core + built-in modules | active |
| loom-smithay | Wayland/DRM/render infrastructure | active |
| loom-nsticky | Sticky/staged windows, scratchpads, workflow | existing, left untouched; loom-companion absorbs its code and offers compatibility aliases/config |
| loom-companion | Automation, rules, run-or-raise, singleton, swallow, orchestration | **delivered**: one daemon + per-context crates; parking, rules, windows, stage/scratchpad, nsticky aliases, swallow, idle/rotate modules and event adapters (187 tests) |
| loom-session | Context/session save/restore | **delivered**: named contexts with outputs, workspaces, windows, hooks, ordered idempotent restore, late-app waiting, monitor/audio/wallpaper/companion profile orchestration with partial success, history/snapshots, import/export, templates and `.desktop`/Flatpak launches (54 tests) |
| loom-sidepanels | Persistent side panels | **delivered**: layer-shell panels on all edges, built-in modules (clock/workspaces/title/media/battery/cpu/memory/network/volume/tray/custom), SNI tray host with pixmap/SVG icons, actions, per-output config, reload (67 tests + 104 stack tests) |
| loom-shot | Capture/recording/annotation | **delivered**: screenshots with HDR modes, full annotation engine (blur/pixelate/redaction/crop/zoom/spotlight/lines), annotation log with undo/redo, capture history, OCR, multi-output stitching, metadata and screen recording (`wf-recorder`, xdg-desktop-portal ScreenCast via gst-launch, and command backends; region/window/output, codecs, audio, PID state, `--stop`) (132 tests) |
| loom-monitor | Output profiles, safe apply | **delivered**: schema v2 with matching, rules, watch, validation, rollback and JSON output, plus the safe-apply confirmation window (`apply --confirm-timeout`/`confirm`/`cancel`), runtime `position`/`arrange` and a GTK drag arrangement editor (80 tests) |
| loom-audio | PipeWire routing | **delivered**: native PipeWire backend (registry, port-link moves, metadata default sink, volume/mute) with pactl/wpctl fallbacks and restore, plus sources, profiles, focus/workspace context rules, hotplug gating, diagnostics and a volume/mute mixer with a GTK mixer (54 tests) |
| loom-wallpaper | Scenes, per-workspace wallpapers | **delivered**: native layer-shell background backend with cover-scaled shm images, reload and command fallback, cross-fade transitions, native video wallpapers with HDR→SDR tone mapping, playlists, time rules and per-session scenes (47 tests) |
| loom-settings | Unified GTK4/libadwaita control center | **delivered**: full niri catalog coverage with byte-preserving KDL edits, dry-run diffs, import/export, plus Overview, Compositor, Displays (profiles, drag arrangement, HDR), Sessions, Wallpaper, Audio (mixer), Side panels, Automation (rules/idle/rotate/marks/history), Services and About through each app's own CLI/JSON (98 tests) |
| loom-portal | XDG portal backend + external layer-shell picker daemon | **delivered**: Screenshot/PickColor native, ScreenCast proxy, picker, install/uninstall, portal descriptor, D-Bus/systemd units and smoke script (42 tests) |
| loom-noctalia-plugins | Shell integration | **delivered**: in-repo Luau plugin source with bar widgets for companion, sticky/stage, monitor, session, shot, audio, wallpaper and sidepanels; manifest + Lua syntax checked, live Noctalia check pending (8 plugins) |

**Do not create repositories or publish anything without explicit approval.**
Piri and niri-sidepanels should preserve upstream history/attribution if reused.

### 4.1 Naming rules and candidates

`loom-*` repository names are only for **independently installable services or
applications**. Internal compositor modules do not get a repository:

- internal modules: `effects`, `navigation`, `cursor`, `agent`, `kde`, `vulkan`;
- companion features that are not yet independent: scratchpads, staging,
  run-or-raise, singleton, swallow, idle handling, auto-rotate.

Candidate projects: `loom-idle` and `loom-rotate` were delivered as
`companion-idle`/`companion-rotate` modules (idle policies/inhibitors and
accelerometer rotation) rather than separate repositories. `loom-desktop`
exists as packaging recipes and `docs/PACKAGING.md`, not as a runtime component.

| Candidate | Purpose | Recommendation |
| --- | --- | --- |
| loom-idle | idle/suspend policy, game/audio inhibition (Stasis reference) | start as a loom-companion module; split only if it needs its own lifecycle |
| loom-rotate | accelerometer-based output rotation (iio-niri reference) | start as a companion module/service |
| loom-desktop | umbrella docs, package profiles, compatibility matrix | documentation only, no runtime dependency |
| loom-noctalia-plugins | Noctalia widgets for the ecosystem | later; shell remains frozen |

Rules:

- Do not create a repository merely to host a small feature.
- Do not create a shared crate (`loom-ipc`, `loom-niri-client`) until at
  least two projects need it.
- Keep one owner per workflow: sticky/stage/scratchpad in loom-companion,
  sessions in loom-session, outputs in loom-monitor, capture in loom-shot.

## 5. Interfaces

### niri IPC

- One persistent event connection per companion.
- Typed actions/events; additive changes; versioned clients.
- A capability query exposing: sticky primitive, virtual outputs, isolated
  outputs, screencopy/ext-image-copy, native portal, agent seat, shader presets,
  per-keyboard config.
- Hidden/parked state must be observable so tools can reconcile after restart.

### Action registry

Both the compositor and loom-companion expose a machine-readable action
registry grouped by category:

- `loom-niri` IPC: module/capability query plus the native actions.
- `loom-companion`: `loomctl actions --json`, UDS `core.api.actions`.
- Each action has a stable hierarchical name (`window.sticky.toggle`,
  `automation.rules.explain`, `compositor.navigation.open-grid`), owner,
  argument schema, permissions and runtime availability.
- Namespaces are grouping nodes; a module owns a leaf subtree. Tools and
  Noctalia plugins query this registry instead of hardcoding feature detection.

### Companion APIs

- Versioned JSON over Unix sockets under `$XDG_RUNTIME_DIR/loom-<name>/`.
- Request IDs, typed errors, subscriptions, capability discovery.
- Socket permissions restricted; no HTTP localhost; no arbitrary shell by
  default.
- Document schema and compatibility.

### Sticky contract

- If `sticky` capability is present: nsticky delegates sticky to native actions.
  Implemented by `contrib/patches/nsticky-native-sticky.patch`; the local
  loom-nsticky checkout has it applied and its 473-test suite passes.
- Otherwise: nsticky uses its move-based fallback.
- Staging/scratchpads remain nsticky; companion may call nsticky rather than
  duplicating it.
- Never run two sticky engines.

### Portals

- Compositor provides buffers/protocols and the picker UI where needed.
- The portal backend lives in `loom-portal` from the start; do not duplicate
  capture paths.

## 6. Roadmap

### Phase 0 — Architecture (this document)

- Approve tiers, module names, default build, portal/agent/sticky decisions.
- Update `MODULES.md`, `ARCHITECTURE.md`, `STATUS.md`.
- Do not create companion repos yet.

### Phase 1 — Finish the compositor core on the elite base

1. Per-keyboard configuration (core input) — branch already started.
2. Biri layout/output features: hidden workspaces, virtual outputs, isolated
   outputs, sticky primitive, float-above-fullscreen, portrait axis.
3. Presentation: tearing, FIFO, async flips, explicit sync, pointer warp.
4. HDR/Vulkan niri-side port.
5. KDE module and demoji privacy fixes.

### Phase 2 — Modularise the elite features

- Move effects, navigation, cursor, agent, portal, KDE behind modules and
  feature gates.
- Add capability query and runtime-off checks.
- Prove zero cost when off and scan-out preservation.

### Phase 3 — Companion foundation

- Audit Piri deeply; preserve history/licence; decide plugin model.
- Build loom-companion: single IPC listener, state sync, reconnection,
  scratchpads/rules/singleton/swallow/run-or-raise.
- Absorb nsticky's staging/scratchpad/workflow code with its history and
  attribution; use the native sticky primitive; no duplicate sticky.
- Keep a compatibility `nsticky` CLI/config path during migration.

### Phase 4 — Session and monitor

- loom-session contexts (windows + workspaces + outputs + hooks).
- loom-monitor with safe apply/rollback and profiles.

### Phase 5 — Productivity and capture

- loom-sidepanels; loom-shot with HDR-aware capture and annotations.

### Phase 6 — Multimedia

- loom-audio (after PWSW audit), loom-wallpaper (reuse Noctalia).

### Phase 7 — Settings and shell integration

- loom-settings GTK4; Noctalia plugins only; no shell.

### Phase 8 — Packaging

- Arch/CachyOS packages, AUR, installation profiles, docs, compatibility matrix.

## 7. Risks and mitigations

- **Module explosion.** Every module boundary adds integration points. Prefer
  core-with-runtime-gate unless there is a heavy dependency or a real security
  boundary.
- **Duplicate sticky/scratchpad logic.** Enforce the sticky contract and one
  owner per workflow.
- **Portal duplication.** Keep one capture path; extract the backend, don't
  clone it.
- **Piri quality/licence.** Audit before adoption; preserve attribution.
- **PWSW is LLM-generated and unreviewed.** Treat as a design reference; do not
  copy without a security/architecture review.
- **Noctalia API churn.** Plugins stay thin; native CLIs/APIs remain the source
  of truth.
- **Shell scope creep.** The shell is frozen; do not build one.
- **Companion availability.** Every feature must degrade gracefully when a
  companion is missing.

## 8. Decisions and remaining questions

Resolved:

1. **Default build**: full binary with all built-in modules enabled; minimal via
   `--no-default-features` and installation profiles.
2. **Portal**: `loom-portal` daemon from the start; the compositor keeps only
   capture plumbing and screenshot/IPC actions.
3. **Agent seat**: module off by default, explicit authorization, restricted
   socket; automation policy in loom-companion.
4. **Sticky/staged/scratchpads**: native primitive in the compositor; workflow
   absorbed into loom-companion with nsticky's history/attribution.
5. **Navigation/minimise**: minimise core; grid/carousel/magnifier in the
   `navigation` module.
6. **Repositories**: companion repositories after Phase 1.
7. **Piri**: architecture reference only; loom-companion is an in-tree
   implementation that vendors no Piri code (§26.11 of the expansion plan).
8. **Shell**: Noctalia plugins only; no native shell.

More resolved:

- **loom-companion shape**: one daemon with internal `sticky`, `stage`,
  `scratchpad`, `rules` modules, modelled on Piri's compiled-plugin idea and
  absorbing nsticky's workflows. It does not vendor Piri code: the daemon's
  core is the JSON/UDS protocol and per-namespace crates, which Piri does not
  provide (see `EXPANSION_PLAN.m` §26.11 for the deviation and rationale).
- **nsticky compatibility**: loom-companion accepts `nsticky` CLI/config as
  aliases. The nsticky repository is **not modified or deprecated**; it remains
  a standalone option for users who want only sticky/staged windows.
- **Portal picker**: external layer-shell picker owned by loom-portal.

Resolved during delivery:

- **loom-idle / loom-rotate**: delivered as `companion-idle`/`companion-rotate`
  modules; split into repositories only if they need their own lifecycle.
- **loom-audio**: native PipeWire backend delivered (registry, port links,
  metadata default sink, volume/mute) with pactl/wpctl fallbacks; PWSW was not
  adopted.
- **loom-shot scope**: screenshots, HDR handling, annotations, history, OCR
  and recording are all in `loom-shot/`.
- **loom-portal scope**: Screenshot and PickColor native, ScreenCast proxies
  niri (the compositor keeps capture and PipeWire ownership); RemoteDesktop is
  not part of this scope.
- **loom-settings**: thin GTK4 frontend plus CLI that delegates to each app's
  own config/CLI; it never owns their data. HDR mode and reference luminance
  are editable in KDL, and the Displays tab also has the runtime selector.
- **nsticky delegation**: `contrib/patches/nsticky-native-sticky.patch` makes
  loom-nsticky use the compositor's native sticky flag when `Capabilities`
  reports it, with the move-based fallback otherwise.


## loom-portal status

`loom-portal/` now exists as a separate daemon inside this repository,
excluded from niri's Cargo workspace. It implements the portal Screenshot
backend (fullscreen/interactive screenshot and colour picking) and proxies the
ScreenCast backend (CreateSession/SelectSources/Start plus Session/Request
Close) to the compositor's existing portal backend, with the PipeWire node ids
passed through. Interactive screenshots use an external layer-shell
`loom-portal-picker`; ScreenCast selection is also external: output picking via the layer-shell
picker, window picking via niri's IPC crosshair, both through niri's
`org.loom.portal.ScreenCastSelection` API.
