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
| Native portal | niri-elite | Portal backend | Module `portal` → later `better-portal` |
| Window groups | Naxdy / upstream tabs | Upstream tabs are the base | Core; Naxdy refinements only if still missing |
| Sticky windows | better-nsticky, biri | Native primitive + nsticky workflow | Core primitive + better-nsticky |
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
| better-nsticky | Sticky/staged windows, scratchpads, workflow | existing |
| better-companion | Automation, rules, run-or-raise, singleton, swallow, orchestration | new, high priority |
| better-session | Context/session save/restore | new, high priority |
| better-sidepanels | Persistent side panels | new, medium |
| better-shot | Capture/recording/annotation | new, medium |
| better-monitor | Output profiles, safe apply | new, medium-high |
| better-audio | PipeWire routing | new, exploratory |
| better-wallpaper | Scenes, per-workspace wallpapers | new, medium |
| better-settings | Unified GTK4 settings frontend | new, medium-high |
| better-portal | Portal backend (after extraction) | candidate |
| better-noctalia-plugins | Shell integration | new, later |

**Do not create repositories or publish anything without explicit approval.**
Piri and niri-sidepanels should preserve upstream history/attribution if reused.

## 5. Interfaces

### niri IPC

- One persistent event connection per companion.
- Typed actions/events; additive changes; versioned clients.
- A capability query exposing: sticky primitive, virtual outputs, isolated
  outputs, screencopy/ext-image-copy, native portal, agent seat, shader presets,
  per-keyboard config.
- Hidden/parked state must be observable so tools can reconcile after restart.

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
- Portal backend can move to `better-portal` once stable; do not duplicate
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
- Integrate nsticky via the sticky capability; no duplicate sticky.

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

## 8. Open questions (need a decision)

1. **Default build**: full binary with all built-in modules enabled (recommended)
   plus `--no-default-features` minimal, or minimal by default?
2. **Portal**: built-in module now, or start `better-portal` as a separate
   process immediately?
3. **Agent seat**: default-off with explicit IPC authorization (recommended), or
   opt-in Cargo feature only?
4. **Scratchpads**: stay in better-nsticky, move to better-companion, or expose
   both through one shared contract?
5. **Navigation/minimise**: should grid/carousel/magnifier be a compile-time
   optional module, while minimise stays core? (Recommended: minimise core;
   navigation default-on module.)
6. **Repository creation**: may we create the companion repositories now, or only
   after Phase 0/1?
7. **Piri**: fork with history, or build a new daemon using Piri as a reference?
   (Recommended: fork/preserve history if the licence allows, then adapt.)
8. **Shell**: confirm Noctalia plugins only and no native shell work for now.
