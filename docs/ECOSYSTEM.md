# Better Desktop ecosystem plan

This document connects better-niri with the wider Better Desktop family. It
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

better-niri + better-smithay + upstream Niri compatibility + Wayland/DRM.

Owns: presentation, HDR/colour, input, layout/window primitives, output
management, capture protocols, sticky primitive, virtual outputs, IPC/config.

### Layer B — Native services

better-nsticky, better-companion, better-session, better-sidepanels, better-shot,
better-monitor, better-audio, better-wallpaper, better-settings, optionally
better-portal.

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
| Native portal | niri-elite | Separate daemon from the start | `better-portal` |
| Window groups | Naxdy / upstream tabs | Upstream tabs are the base | Core; Naxdy refinements only if still missing |
| Sticky windows | better-nsticky, biri | Native primitive in core; workflow absorbed into better-companion | Core primitive + better-companion |
| Focus-ring shaders | biri | Scope of `effects` | Module `effects` |
| Scratchpads | Piri, niri-scratchpad-rs, nsticky | Companion feature | better-companion / nsticky |
| Run-or-raise, singleton, swallow | Piri, niri-ror | Companion automation | better-companion |
| Side panels | niri-sidepanels | Reuse/adapt | better-sidepanels |
| Sessions | nirinit, niri-session-manager, niri-companion | Combine into contexts | better-session |
| Config GUI | NiriMod, nirimon, nirilayout | Reference; preserve KDL/comments | better-settings / better-monitor |
| Audio routing | PWSW | Audit first (LLM-generated, author warning) | better-audio |
| Keyboard layout per device | kunai | Our per-keyboard config covers most | Core (`input`); kunai as reference |
| Auto-rotate | iio-niri | Optional service | better-companion integration |
| Idle/suspend | Stasis | Optional service | better-companion or separate `better-idle` |
| Screenshots/recording | niri-shot, nirisnap, utsushot | Compare and reuse | better-shot |
| Shell | Noctalia, DMS, Exo, iNiR, Ironbar, Glimpse | Shell frozen | Noctalia plugins only |

## 4. Repositories and responsibilities

| Repository | Responsibility | Status |
| --- | --- | --- |
| better-niri | Compositor, core + built-in modules | active |
| better-smithay | Wayland/DRM/render infrastructure | active |
| better-nsticky | Sticky/staged windows, scratchpads, workflow | existing, left untouched; better-companion absorbs its code and offers compatibility aliases/config |
| better-companion | Automation, rules, run-or-raise, singleton, swallow, orchestration | new, high priority |
| better-session | Context/session save/restore | new, high priority |
| better-sidepanels | Persistent side panels | new, medium |
| better-shot | Capture/recording/annotation | new, medium |
| better-monitor | Output profiles, safe apply | new, medium-high |
| better-audio | PipeWire routing | new, exploratory |
| better-wallpaper | Scenes, per-workspace wallpapers | new, medium |
| better-settings | Unified GTK4 settings frontend | new, medium-high |
| better-portal | XDG portal backend + external layer-shell picker daemon | committed, after Phase 1 |
| better-noctalia-plugins | Shell integration | new, later |

**Do not create repositories or publish anything without explicit approval.**
Piri and niri-sidepanels should preserve upstream history/attribution if reused.

### 4.1 Naming rules and candidates

`better-*` repository names are only for **independently installable services or
applications**. Internal compositor modules do not get a repository:

- internal modules: `effects`, `navigation`, `cursor`, `agent`, `kde`, `vulkan`;
- companion features that are not yet independent: scratchpads, staging,
  run-or-raise, singleton, swallow, idle handling, auto-rotate.

Candidate projects, not yet committed:

| Candidate | Purpose | Recommendation |
| --- | --- | --- |
| better-idle | idle/suspend policy, game/audio inhibition (Stasis reference) | start as a better-companion module; split only if it needs its own lifecycle |
| better-rotate | accelerometer-based output rotation (iio-niri reference) | start as a companion module/service |
| better-desktop | umbrella docs, package profiles, compatibility matrix | documentation only, no runtime dependency |
| better-noctalia-plugins | Noctalia widgets for the ecosystem | later; shell remains frozen |

Rules:

- Do not create a repository merely to host a small feature.
- Do not create a shared crate (`better-ipc`, `better-niri-client`) until at
  least two projects need it.
- Keep one owner per workflow: sticky/stage/scratchpad in better-companion,
  sessions in better-session, outputs in better-monitor, capture in better-shot.

## 5. Interfaces

### niri IPC

- One persistent event connection per companion.
- Typed actions/events; additive changes; versioned clients.
- A capability query exposing: sticky primitive, virtual outputs, isolated
  outputs, screencopy/ext-image-copy, native portal, agent seat, shader presets,
  per-keyboard config.
- Hidden/parked state must be observable so tools can reconcile after restart.

### Action registry

Both the compositor and better-companion expose a machine-readable action
registry grouped by category:

- `better-niri` IPC: module/capability query plus the native actions.
- `better-companion`: `companionctl actions --json`, UDS `core.api.actions`.
- Each action has a stable hierarchical name (`window.sticky.toggle`,
  `automation.rules.explain`, `compositor.navigation.open-grid`), owner,
  argument schema, permissions and runtime availability.
- Namespaces are grouping nodes; a module owns a leaf subtree. Tools and
  Noctalia plugins query this registry instead of hardcoding feature detection.

### Companion APIs

- Versioned JSON over Unix sockets under `$XDG_RUNTIME_DIR/better-<name>/`.
- Request IDs, typed errors, subscriptions, capability discovery.
- Socket permissions restricted; no HTTP localhost; no arbitrary shell by
  default.
- Document schema and compatibility.

### Sticky contract

- If `sticky` capability is present: nsticky delegates sticky to native actions.
- Otherwise: nsticky uses its move-based fallback.
- Staging/scratchpads remain nsticky; companion may call nsticky rather than
  duplicating it.
- Never run two sticky engines.

### Portals

- Compositor provides buffers/protocols and the picker UI where needed.
- The portal backend lives in `better-portal` from the start; do not duplicate
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
- Build better-companion: single IPC listener, state sync, reconnection,
  scratchpads/rules/singleton/swallow/run-or-raise.
- Absorb nsticky's staging/scratchpad/workflow code with its history and
  attribution; use the native sticky primitive; no duplicate sticky.
- Keep a compatibility `nsticky` CLI/config path during migration.

### Phase 4 — Session and monitor

- better-session contexts (windows + workspaces + outputs + hooks).
- better-monitor with safe apply/rollback and profiles.

### Phase 5 — Productivity and capture

- better-sidepanels; better-shot with HDR-aware capture and annotations.

### Phase 6 — Multimedia

- better-audio (after PWSW audit), better-wallpaper (reuse Noctalia).

### Phase 7 — Settings and shell integration

- better-settings GTK4; Noctalia plugins only; no shell.

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
2. **Portal**: `better-portal` daemon from the start; the compositor keeps only
   capture plumbing and screenshot/IPC actions.
3. **Agent seat**: module off by default, explicit authorization, restricted
   socket; automation policy in better-companion.
4. **Sticky/staged/scratchpads**: native primitive in the compositor; workflow
   absorbed into better-companion with nsticky's history/attribution.
5. **Navigation/minimise**: minimise core; grid/carousel/magnifier in the
   `navigation` module.
6. **Repositories**: companion repositories after Phase 1.
7. **Piri**: fork preserving history and attribution.
8. **Shell**: Noctalia plugins only; no native shell.

More resolved:

- **better-companion shape**: one daemon with internal `sticky`, `stage`,
  `scratchpad`, `rules` modules, forked from Piri with nsticky's history
  absorbed. First delivery is the IPC/state base plus nsticky's workflows.
- **nsticky compatibility**: better-companion accepts `nsticky` CLI/config as
  aliases. The nsticky repository is **not modified or deprecated**; it remains
  a standalone option for users who want only sticky/staged windows.
- **Portal picker**: external layer-shell picker owned by better-portal.

Remaining questions:

- **better-idle / better-rotate**: companion modules first, or independent
  repositories from the start? (Recommended: companion first.)
- **better-audio**: audit PWSW and then decide between adapting it and a
  WirePlumber-native rule engine.
- **better-shot scope**: screenshots + annotation first, recording/OCR later?
- **better-portal scope**: ScreenCast + Screenshot first, RemoteDesktop later?
- **better-settings**: thin GTK4 frontend that delegates to each app's
  validated config API (recommended) rather than owning configuration itself.


## better-portal status

`better-portal/` now exists as a separate daemon inside this repository,
excluded from niri's Cargo workspace. It implements the portal Screenshot
backend (fullscreen/interactive screenshot and colour picking) by delegating to
the compositor's GNOME-Shell-style interfaces, with a cancellable
`org.freedesktop.impl.portal.Request`. ScreenCast extraction and the external
layer-shell picker remain.
