--!nonstrict
-- Bar widget: nsticky-style workflow through better-companion.
-- Left click toggles staging for the focused window, right click toggles the
-- configured scratchpad slot, middle click lists staged windows.

local function run(args)
  noctalia.runAsync(args, function(result)
    if result.exitCode ~= 0 then
      noctalia.notifyError("Better Sticky", result.stderr or "companion call failed")
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
  if not noctalia.commandExists("companionctl") then
    noctalia.notifyError("Better Sticky", "companionctl is not installed")
    return
  end
  run({ "companionctl", "call", "stage", "toggle", "{}" })
end

function onRightClick()
  if not noctalia.commandExists("companionctl") then
    noctalia.notifyError("Better Sticky", "companionctl is not installed")
    return
  end
  local slot = noctalia.getConfig("scratchpad") or "term"
  run({ "companionctl", "call", "scratchpad", "toggle", '{"name":"' .. slot .. '"}' })
end

function onMiddleClick()
  noctalia.runInTerminal("companionctl call stage list")
end
