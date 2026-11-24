--!nonstrict
-- Bar widget: better-companion daemon status. Left click opens the action
-- registry in a terminal, right click opens the companion README.

local label = "companion"

local function set_state(ok, text)
  label = text
  if ok then
    barWidget.setColor("primary", "script")
  else
    barWidget.setColor("error", "script")
  end
  barWidget.setText(label)
end

local function refresh()
  if not noctalia.commandExists("companionctl") then
    set_state(false, "companion?")
    return
  end
  noctalia.runAsync({ "companionctl", "status" }, function(result)
    if result.exitCode == 0 then
      set_state(true, "companion")
    else
      set_state(false, "companion!")
    end
  end)
end

function update()
  noctalia.setUpdateInterval(10000)
  barWidget.setGlyph("robot")
  barWidget.setTooltip("better-companion daemon")
  refresh()
end

function onClick()
  if noctalia.commandExists("companionctl") then
    noctalia.runInTerminal("companionctl describe")
  else
    noctalia.notifyError("Better Companion", "companionctl is not installed")
  end
end

function onRightClick()
  noctalia.runAsync({ "better-settings" })
end

function onIpc(event)
  if event == "refresh" then
    refresh()
  elseif event == "click" then
    onClick()
  elseif event == "right-click" then
    onRightClick()
  end
end
