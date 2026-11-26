--!nonstrict
-- Bar widget: total tracked desktop time. Left click opens the per-app ranking
-- in a terminal, right click opens the companion command reference.

local function format_total(seconds)
  local minutes = math.floor((seconds or 0) / 60)
  if minutes < 60 then
    return string.format("%dm", minutes)
  end
  return string.format("%dh%02dm", math.floor(minutes / 60), minutes % 60)
end

local function refresh()
  if not noctalia.commandExists("companionctl") then
    barWidget.setText("time?")
    return
  end
  noctalia.runAsync({ "companionctl", "call", "screentime", "status" }, function(result)
    if result.exitCode ~= 0 then
      barWidget.setText("time!")
      return
    end
    local value = noctalia.json.decode(result.stdout)
    barWidget.setText(format_total(value and value.total_seconds))
  end)
end

function update()
  noctalia.setUpdateInterval(60000)
  barWidget.setGlyph("clock")
  barWidget.setTooltip("Tracked desktop time by better-companion")
  refresh()
end

function onClick()
  if noctalia.commandExists("companionctl") then
    noctalia.runInTerminal({ "companionctl", "call", "screentime", "top" })
  else
    noctalia.notifyError("Better Screen Time", "companionctl is not installed")
  end
end

function onRightClick()
  if noctalia.commandExists("companionctl") then
    noctalia.runInTerminal("companionctl describe")
  end
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
