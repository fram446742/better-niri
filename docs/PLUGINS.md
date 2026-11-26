# Plugin runtime architecture and feasibility

This document evaluates the hybrid plugin model for loom-companion:

- Piri's compiled `Plugin` trait as the **internal contract**;
- a runtime-neutral plugin manager (registry, categories, permissions,
  overrides, diagnostics);
- adapters for compiled modules, native `.so`, WASM and external services.

The goal is to keep one plugin system, not four independent ones.

## 1. Decision summary

Recommended:

1. **`Piri::Plugin` stays private.** Do not expose it as a binary ABI. It is one
   adapter's internal trait, not the public contract.
2. **Define a runtime-neutral host interface** (`PluginHost`/`PluginInstance`)
   that every adapter implements. The registry, action categories, event
   filtering, overrides, permissions and diagnostics live above it.
3. **Ship v1 with compiled modules + external UDS services.** These are proven
   and cover first-party features and integrations.
4. **Prototype native ABI and WASM behind experimental features.** Do not ship
   them until the benchmark and security review pass.
5. **Trust levels are explicit:**
   - compiled first-party: full trust, in-process;
   - native `.so`: trusted code, in-process, can crash the daemon; distro/signed
     only, never auto-downloaded;
   - WASM: untrusted, sandboxed, capability-limited;
   - UDS/D-Bus: separate process, filesystem/socket permissions.

## 2. Runtime-neutral interface

```
PluginManager
  ├── Registry        action categories, ownership, versioning
  ├── Event bus       typed events, subscriptions, coalescing
  ├── Override engine pass/block/rewrite/before/after
  ├── Permissions     capabilities per plugin
  ├── Diagnostics     status, explain, dry-run, metrics
  └── Adapters
        ├── CompiledAdapter     Piri Plugin trait (in-process)
        ├── NativeAbiAdapter    C ABI / xabi (in-process)
        ├── WasmAdapter         Wasmtime + WIT (sandbox)
        └── ServiceAdapter      UDS / D-Bus (out-of-process)
```

Each adapter converts its runtime into the same host-side operations:

- list actions/categories;
- handle an action request with typed arguments;
- receive subscribed events;
- report status/errors;
- load/unload/reload;
- expose capabilities/permissions.

The manager never talks to a runtime directly. Swapping xabi for a hand-written
C ABI, or Wasmtime for another runtime, does not change the registry, action
names or config.

## 3. Feasibility per runtime

### 3.1 Compiled modules (Piri model) — production-ready

Piri (`fdfa170`, MIT) implements exactly this: a `Plugin` trait
(`src/plugins/mod.rs`), a macro-based registry, one Niri event listener, event
interest filtering, config hot reload and IPC command dispatch. No `libloading`,
WASM or dynamic ABI.

- **Pros:** fastest, type-safe, simple, no serialization.
- **Cons:** adding a module requires rebuilding the daemon; third parties cannot
  install code at runtime.
- **Use:** all first-party modules and everything on the event hot path.

### 3.2 C ABI `.so` — viable, expensive to design

A versioned C ABI with opaque handles and a vtable is the most portable native
option. It is stable across Rust versions and languages.

- **Costs:** ownership/allocator rules, string/bytes representation, error
  propagation, panic guards, async/polling, callbacks, threading, version
  negotiation. All must be designed and tested.
- **Risk:** a segfault in a native plugin kills the daemon; no sandbox.
- **Use:** trusted, performance-critical plugins only.

### 3.3 xabi — promising for Rust-native plugins

`xabi` generates a stable, C-compatible ABI from Rust traits: vtable, manifest,
panic guards, async polling glue, host handles, trait/ABI version checks. It is
explicitly **not** a plugin framework; registry, discovery, permissions and
lifecycle remain ours.

- **Pros:** keeps Rust traits ergonomically; checks manifest/version before
  calls; catches panics at the boundary; supports async traits.
- **Cons:** young project; generated ABI snapshots must be tested; native code is
  trusted and in-process; not a substitute for process isolation.
- **Verdict:** the best candidate for trusted Rust plugins, **after a prototype
  and ABI snapshot tests**. Do not make it the only native ABI; keep the adapter
  boundary so a C ABI can replace it.

### 3.4 savefile-abi — viable but not for hot paths

`savefile-abi` provides ABI-stable Rust-to-Rust FFI for dynamic libraries,
supporting different rustc versions and trait objects. It falls back to
serialization when layouts cannot be proven identical, with heap allocation and
versioning costs.

- **Pros:** ergonomic Rust traits, versioned data structures, trait objects.
- **Cons:** serialization/allocation overhead; restrictions on argument types;
  designed for plugin calls, not high-frequency event fan-out.
- **Verdict:** benchmark it, but expect it to lose to xabi/C ABI on event
  throughput. Keep it as a comparison, not the default.

### 3.5 Wasmtime + WIT — viable for untrusted third-party plugins

Wasmtime's component model with WIT gives typed, language-neutral interfaces,
sandboxing, memory/CPU limits (fuel/epoch), and no ambient capabilities unless
granted. WASI preview 2/0.3 is the modern toolchain (`wasm32-wasip2`).

- **Pros:** real isolation; language-agnostic; per-plugin capabilities; traps are
  recoverable; no ABI coupling to the daemon.
- **Cons:** boundary copies; guest runtimes add size/startup; async/event
  bridging; WIT/toolchain churn; debugging is harder; not free for chatty
  per-event calls.
- **Use:** untrusted third-party plugins, UI-adjacent extensions, integrations
  that must not crash the daemon.
- **Design rule:** coarse-grained calls and batched events, not one WASM call per
  keypress.

### 3.6 External services (UDS/D-Bus) — production-ready

- **Pros:** process isolation, independent release/language, permissions via
  sockets, simple failure handling.
- **Cons:** highest latency, schema/versioning, state resync, another process to
  supervise.
- **Use:** heavy integrations (audio, monitor, wallpaper, session, portal) and
  third-party tools; not per-frame or high-frequency window events.

## 4. Recommended trust and runtime matrix

| Plugin kind | Runtime | Trust | Hot path | Distribution |
| --- | --- | --- | --- | --- |
| First-party modules | compiled | full | yes | in the binary |
| Trusted native extension | xabi (preferred) / C ABI | full, in-process | yes | distro/signed, no auto-download |
| Untrusted third-party | WASM + WIT | sandboxed | batched/coarse | user-installed `.wasm` |
| Integration/service | UDS/D-Bus | process-isolated | no | independent package |

Never load an unsigned native library from a user-writable directory by default.
Native plugins are code execution with the daemon's privileges.

## 5. Benchmark prototype (before committing)

Build a standalone prototype workspace (not shipped) with the same trivial
plugin in six forms:

1. compiled Piri module (baseline);
2. hand-written C ABI `.so`;
3. xabi `.so`;
4. savefile-abi `.so`;
5. Wasmtime/WIT component;
6. UDS service.

### Workloads

- no-op action call;
- action with 64 B, 1 KiB and 64 KiB payloads;
- event fan-out: 1, 10 and 100 subscribers;
- burst: 1, 1k, 10k, 100k events (window focus/change/workspace/output);
- state query and config reload;
- load/unload/reload;
- panic/error/trap/crash recovery.

### Metrics

- p50/p95/p99 per-call latency; batched throughput;
- allocations and bytes copied per call/event;
- steady-state RSS and peak RSS; daemon binary size;
- plugin load and hot-reload time;
- behavior across rustc versions (ABI compatibility);
- crash containment (native segfault vs WASM trap vs UDS crash);
- CPU share during a realistic desktop trace.

### Acceptance targets (initial, to refine)

- Compiled baseline is the reference.
- Native ABI adds no more than ~2× a small action call, and sustains the event
  burst without dropping a frame at 60/144 Hz.
- WASM is acceptable only if event batching keeps added frame time under the
  budget; if not, restrict it to coarse actions.
- UDS is for integrations; do not put per-keypress events on it.
- No runtime may block the compositor/companion event loop; all calls are
  bounded or offloaded.

## 6. Event delivery

- One event source (niri IPC), normalized into typed events.
- Subscription filters per plugin; no broadcast to uninterested plugins.
- Coalescing for high-frequency events (window changed, pointer); versioned
  snapshots for state.
- Bounded queues with backpressure; slow plugins are throttled or disconnected.
- Event order is per-source deterministic; document any reordering.
- Never call a plugin while holding a lock on core state.

## 7. Permissions and security

- Capability manifests per plugin: events, action categories, filesystem,
  process spawn, input injection, notifications.
- Native plugins: full trust; cannot be sandboxed; explicit user/admin consent.
- WASM: capability-based host imports only; fuel/epoch limits; memory limits.
- UDS: `0600` sockets, peer credentials, request size limits, schema validation.
- No arbitrary shell execution by default; allowlists and dry-run.
- Audit logging for overrides, spawns, input injection and permission changes.

## 8. Versioning

- Host API version and action schema are separate from plugin ABI version.
- Native: trait ID + ABI version + generated layout snapshots; reject mismatches
  before calling.
- WASM: WIT package versions; reject unknown imports.
- UDS: JSON protocol version + capability discovery.
- The registry records plugin version, runtime, capabilities and status.

## 9. Recommendation

Proceed with the hybrid architecture, but in stages:

1. **v1:** internal runtime-neutral manager + compiled modules + UDS services.
2. **v1.5 experimental:** xabi and Wasmtime adapters behind feature flags; run
   the benchmark and security review.
3. **v2:** enable native ABI for trusted plugins and WASM for untrusted plugins
   if the numbers and isolation hold.
4. Do not expose `Piri::Plugin` as a public ABI at any stage.
5. Keep the C ABI design in reserve for non-Rust native plugins.
