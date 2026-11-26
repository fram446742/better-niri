# Remaining work

The reference-port round is in the tree. What follows is the small set of items
that are not code ports: hardware validation, one deliberately rejected ABI, and
two platform limits that belong to the compositor or the shell rather than to a
companion.

## 1. Hardware validation

The HDR scanout path, NVIDIA frame-fence wait, VRR/tearing, multi-GPU scan-out
and suspend/resume cleanup are implemented and covered by headless/Vulkan tests,
but their KMS behaviour needs a real DRM seat. In particular, the post-blend PQ
encode and the HDR cursor plane target NVIDIA hardware that is not available in
this environment. Do not report those paths as validated until the matrix in
`TESTING.md` has been run and its output recorded under `docs/validation/`.

The same applies to a live Spectacle or Plasma portal run for
`org.kde.KWin.ScreenShot2`; the private-bus fd round trip is the current
evidence.

## 2. Piri compiled plugins

Piri's plugin model is an in-process Rust trait, not an ABI. Reusing its plugins
would mean embedding Piri and replacing loom-companion's UDS/JSON boundary;
the plugins would still be compiled per binary, so users would not gain an
installable plugin format. loom-companion keeps the Piri behaviour (rules,
run-or-raise, scratchpads, swallowing, marks) as original code.

If compiled plugin reuse becomes a hard requirement, it should be an optional
`piri-compat` adapter, not the core daemon. Piri is MIT, so attribution and
notices are preserved in `ATTRIBUTION.md`.

## 3. Limits that belong elsewhere

- **Output mirroring.** niri has no mirror primitive. loom-monitor validates
  the relationship and can run it through `wl-mirror`; a compositor-native
  mirror would need an output-composition feature. See `docs/HDR.md` and the
  loom-monitor README.
- **HDR wallpaper output.** HDR video now takes the 10-bit PQ/BT.2020 path with
  a `wp_color_manager_v1` description when the output is HDR-active; static
  images remain SDR, and a missing protocol or 10-bit buffer falls back to SDR
  tone-mapping. The live handshake and scanout still need a hardware run; see
  `docs/HDR.md`.
- **Live Noctalia checks.** The plugin source is syntax- and manifest-checked;
  exercising the widgets needs a running Noctalia session and remains a manual
  check in `docs/validation/noctalia-live-2026-11-24.md`.

Everything else in `ECOSYSTEM_RESEARCH.md` is integrated, recorded as rejected
with the conditions that would make us reconsider it, or kept as a
reference-only project that we do not ship.
