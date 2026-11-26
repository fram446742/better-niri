--!nonstrict
-- Bar widget: pick and restore a loom-session context.
-- Left click restores the selected context, scroll changes the selection,
-- right click opens the context list in a terminal.

local contexts = {}
local index = 1

local function refresh()
  if not noctalia.commandExists("loom-session") then
    barWidget.setText("session?")
    return
  end
  noctalia.runAsync({ "loom-session", "list" }, function(result)
    local value = noctalia.json.decode(result.stdout)
    contexts = {}
    if type(value) == "table" then
      for _, name in ipairs(value) do
        if type(name) == "string" then
          table.insert(contexts, name)
        end
      end
    end
    if #contexts == 0 then
      barWidget.setText("session")
      barWidget.setColor("on_surface", "script")
    else
      if index > #contexts then
        index = 1
      end
      barWidget.setText(contexts[index])
      barWidget.setColor("primary", "script")
    end
  end)
end

local function restore(name)
  noctalia.runAsync({ "loom-session", "restore", name }, function(result)
    if result.exitCode ~= 0 then
      noctalia.notifyError("Loom Session", result.stderr or ("could not restore " .. name))
    else
      noctalia.notify("Loom Session", "restored " .. name)
    end
  end)
end

function update()
  noctalia.setUpdateInterval(30000)
  barWidget.setGlyph("stack-2")
  barWidget.setTooltip("loom-session contexts")
  refresh()
end

function onClick()
  if #contexts == 0 then
    refresh()
    return
  end
  restore(contexts[index])
end

function onRightClick()
  if noctalia.commandExists("loom-session") then
    noctalia.runInTerminal("loom-session list")
  end
end

function onScroll(_axis, steps, startsGesture)
  if not startsGesture or #contexts == 0 then
    return
  end
  index = ((index - 1 + steps) % #contexts) + 1
  barWidget.setText(contexts[index])
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
