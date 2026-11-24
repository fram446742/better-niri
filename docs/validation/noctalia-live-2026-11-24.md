# Live Noctalia plugin check (2026-11-24)

Environment: Noctalia `v5.6.0` running on the user's niri session (PID 6805),
plugin API range includes the declared `plugin_api = 24`.

## Procedure

```sh
cp ~/.config/noctalia/plugins.json /tmp/noctalia-plugins-backup.json
noctalia msg plugins source add better-desktop path \
  /home/franc/Projects/better-niri/better-niri/better-noctalia-plugins
for p in better-audio better-companion better-monitor better-session \
         better-shot better-sidepanels better-sticky better-wallpaper; do
  noctalia msg plugins enable better-desktop/$p
done
noctalia msg plugins list | grep better-desktop
```

## Result

All eight plugins were discovered, exported and enabled:

```
better-desktop/better-audio [better-desktop] 1.0.0 enabled
better-desktop/better-companion [better-desktop] 1.0.0 enabled
better-desktop/better-monitor [better-desktop] 1.0.0 enabled
better-desktop/better-session [better-desktop] 1.0.0 enabled
better-desktop/better-shot [better-desktop] 1.0.0 enabled
better-desktop/better-sidepanels [better-desktop] 1.0.0 enabled
better-desktop/better-sticky [better-desktop] 1.0.0 enabled
better-desktop/better-wallpaper [better-desktop] 1.0.0 enabled
```

`~/.cache/noctalia/noctalia.log.1` records `loaded plugin ...` for every entry
and contains no `[luau] plugin better-desktop/...` errors after the load. The
entries are widgets, so they only execute `onClick`/`update` once placed on a
bar; the load path compiles and runs each entry's top-level chunk, which is the
part that can fail before a user adds the widget.

## Cleanup

The source was removed and every plugin disabled afterwards; the original
`plugins.json` was restored byte-for-byte (`diff` empty) so the user's shell is
unchanged. A live click/update check remains part of the manual desktop pass,
together with the hardware validation.
