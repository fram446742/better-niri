--!nonstrict
-- Bar widget: nsticky-style workflow through loom-companion.
-- Left click toggles staging for the focused window, right click toggles the
-- configured scratchpad slot, middle click lists staged windows.

local function run(args)
  noctalia.runAsync(args, function(result)
    if result.exitCode ~= 0 then
      noctalia.notifyError("Loom Sticky", result.stderr or "companion call failed")
    end
  end)
end

function update()
  noctalia.setUpdateInterval(60000)
  barWidget.setGlyph("pin")
  barWidget.setText("")
  barWidget.setTooltip("click = stage/unstage, right click = scratchpad")
end

function onClick()
  if not noctalia.commandExists("loomctl") then
    noctalia.notifyError("Loom Sticky", "loomctl is not installed")
    return
  end
  run({ "loomctl", "call", "stage", "toggle", "{}" })
end

function onRightClick()
  if not noctalia.commandExists("loomctl") then
    noctalia.notifyError("Loom Sticky", "loomctl is not installed")
    return
  end
  local slot = noctalia.getConfig("scratchpad") or "term"
  run({ "loomctl", "call", "scratchpad", "toggle", '{"name":"' .. slot .. '"}' })
end

function onMiddleClick()
  noctalia.runInTerminal("loomctl call stage list")
end

function onIpc(event)
  if event == "refresh" then
    refresh()
  elseif event == "click" then
    onClick()
  end
end
