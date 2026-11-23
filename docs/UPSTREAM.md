# Syncing with upstream niri

The goal is that `git fetch upstream && git merge upstream/main` stays boring.
This document is the procedure and the known conflict map.

## 1. Remotes

better-niri keeps a direct relationship with niri:

```sh
git remote -v
# origin    https://github.com/fram446742/better-niri.git
# upstream  https://github.com/niri-wm/niri.git
```

better-smithay keeps the same relationship with Smithay:

```sh
# origin    https://github.com/fram446742/better-smithay.git
# upstream  https://github.com/Smithay/smithay.git
```

Reference-only remotes (`mekcck`, `losnoco`, etc.) may exist locally for audits
but must not be build dependencies.

## 2. Checklist before merging upstream

1. `git status` is clean; you are on `main`.
2. `git fetch upstream main`.
3. Record the current state:
   ```sh
   git log -1 --oneline
   git rev-list --count HEAD..upstream/main   # how far behind
   ```
4. Run the drift check against the new upstream **before** merging, to see
   what changed in config/IPC:
   ```sh
   contrib/check-upstream-drift.sh upstream/main
   ```
5. Note upstream changes in the areas listed in section 4.

## 3. Merge procedure

```sh
git switch main
git merge upstream/main          # merge, never rebase main
```

Rules:

- **Never rebase `main`.** Published history and feature provenance must survive.
- **Never force-push `main`.**
- **Upstream wins on structure.** If upstream reworked input handling or a layout
  function, take upstream's shape and re-express our feature on top. Do not keep
  an old fork structure because it is convenient.
- **Do not reformat files.** Resolve conflicts by hand; no `cargo fmt` over whole
  upstream files.
- **Do not combine unrelated features in a conflict resolution.** If a conflict
  touches three features, resolve in three commits if possible.
- Keep merge commits; they are useful provenance.

After resolving:

```sh
LIBCLANG_PATH=/usr/lib cargo check --workspace
LIBCLANG_PATH=/usr/lib cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings   # when the tree is clean
cargo fmt --check                                        # see rule above
```

Then run the drift check again and the headless visual tests:

```sh
contrib/check-upstream-drift.sh upstream/main
cargo test -p niri-visual-tests
```

## 4. Known conflict map

These are the files/functions where our features are expected to collide with
upstream. When adding code, prefer a new file over editing these.

| Area | Upstream file / region | Why it conflicts | Mitigation |
| --- | --- | --- | --- |
| Render entry | `src/niri.rs` `render_inner`, `render_to_vec`, `render_pointer` | Every effect/overlay adds elements here. | One effect-hook call, feature element construction in its own module. |
| Output state | `src/niri.rs` `OutputState` | HDR, shaders, feedback, isolated outputs all add state. | One `OutputEffects` sub-struct; avoid many flat fields. |
| Redraw scheduling | `src/niri.rs` `unfinished_animations_remain`, `redraw`; `src/backend/tty.rs` post-submit | Shader/minimize/grid animations need redraws. | A single `effects_need_redraw()` helper. |
| Input dispatch | `src/input/mod.rs` action match, grabs, keyboard handling | Per-keyboard, binds, gestures, pointer warp. | Add actions via enum; grabs in new files; re-express on upstream input shape. |
| Layout | `layout/mod.rs`, `scrolling.rs`, `monitor.rs`, `workspace.rs`, `tile.rs`, `floating.rs` | Minimize, grid, hidden workspaces, sticky, vertical axis, carousel all alter geometry/state. | Port biri's freshest commits; keep layout hooks minimal; extensive headless tests. |
| Config | `niri-config/src/lib.rs` | Every option registration. | New domain modules; only `pub mod`/re-export lines in `lib.rs`. |
| IPC | `niri-ipc/src/lib.rs`, `src/ipc/*` | New actions/events/fields. | Additive variants; one dispatch arm. |
| Backend TTY | `src/backend/tty.rs` | HDR scanout, tearing, Vulkan, cursor plane, direct scanout. | Isolate in backend helpers; one pin bump at a time; test each driver fallback. |
| Screencasting | `src/screencasting/*`, image-copy handlers | HDR→SDR, shaders-in-capture, portal picker. | Use `RenderTarget`; no second render path. |
| Default config | `resources/default-config.kdl` | Duplicate binds break `can_create_default_config`. | Run `cargo test -p niri-config`; keep additions grouped. |

`src/niri.rs` and `src/input/mod.rs` are the two highest-churn files upstream
(132 and 108 changes in the last 800 commits). Treat edits there as expensive.

## 5. Updating better-smithay

When upstream Smithay moves:

1. Fetch upstream into better-smithay.
2. Merge upstream into better-smithay `master`; keep feature commits.
3. Drop any of our commits that upstream has landed; note the upstream commit in
   the merge message.
4. Build Smithay with the same features niri uses, including `renderer_vulkan`
   when the Vulkan feature is enabled.
5. Run niri's `cargo check` against the new revision using the local patch
   command in `COMPATIBILITY.md`.
6. Bump **both** `rev` lines in better-niri's `Cargo.toml`, and update
   `Cargo.lock` with a normal `cargo update -p smithay`.
7. Record the new combination in `COMPATIBILITY.md`.

Never update `smithay` without `smithay-drm-extras`, and never point niri at two
different Smithay revisions in one build.

## 6. Integration-point registry

Every unavoidable edit to an upstream-owned file should be listed here as it is
added. This makes conflict resolution mechanical and lets us shrink the list over
time. Format: file, function, purpose, feature, owner commit.

| File | Function / region | Purpose | Feature | Notes |
| --- | --- | --- | --- | --- |
| `Cargo.toml` | workspace Smithay deps | Pin better-smithay | infrastructure | Both `smithay` and `smithay-drm-extras`. |
| `niri-ipc/src/lib.rs` | `Action` enum | New IPC actions | all interactive features | Additive. |
| `niri-config/src/lib.rs` | `Config` struct + `pub mod` | Register option modules | all config features | Keep to declarations. |
| `src/handlers/mod.rs` | `delegate_dispatch!` list | Register protocol handlers | protocols | One line per protocol. |
| `src/niri.rs` | `State` fields / `Niri::new` | Register globals/state | protocols, portals | Prefer sub-structs. |

This table will grow during integration; it is the authoritative list for
reviewing upstream merges.

## 7. Simulated sync result (baseline)

At audit time better-niri is exactly upstream `02ee0585`; a merge is a no-op and
there are zero conflicts. better-smithay is exactly Smithay `a38bb709`, the
revision niri pins.

The first real sync simulation will be run after the Smithay consolidation and
the first two feature series. Expected outcome from the conflict map:

- Smithay merge onto a newer upstream: low file-level conflicts (3 in the
  losnoco prototype), but API adaptation work in colour management and Vulkan.
- Niri HDR/presentation series: medium conflicts in `backend/tty.rs` and
  `niri.rs`.
- Layout series (biri): low today (2 commits behind), high in six months unless
  it is ported early.
- KDE/portal: low to medium, mostly additive files.

## 8. Emergency rollback

- Revert the merge commit: `git revert -m 1 <merge>`; do not reset published
  history.
- Revert a feature series by reverting its commits in reverse order.
- If a Smithay pin breaks the build, restore the previous pair of `rev` lines and
  `Cargo.lock`, then investigate on a branch.
