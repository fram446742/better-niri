--!nonstrict
-- Bar widget: left click opens niri's interactive screenshot UI through
-- loom-shot, right click captures the focused output into the clipboard.

local function run(args)
  noctalia.runAsync(args, function(result)
    if result.exitCode ~= 0 then
      noctalia.notifyError("Loom Shot", result.stderr or "capture failed")
    end
  end)
end

function update()
  noctalia.setUpdateInterval(30000)
  barWidget.setGlyph("camera")
  barWidget.setText("")
  barWidget.setTooltip("loom-shot: click = region UI, right click = screen to clipboard")
end

function onClick()
  if not noctalia.commandExists("loom-shot") then
    noctalia.notifyError("Loom Shot", "loom-shot is not installed")
    return
  end
  run({ "loom-shot", "ui" })
end

function onRightClick()
  if not noctalia.commandExists("loom-shot") then
    noctalia.notifyError("Loom Shot", "loom-shot is not installed")
    return
  end
  run({ "loom-shot", "screen", "--copy" })
end

function onIpc(event)
  if event == "refresh" then
    refresh()
  elseif event == "click" then
    onClick()
  end
end
