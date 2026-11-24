--!nonstrict
-- Bar widget: default sink via better-audio. Click/scroll cycles sinks,
-- right click restores the routing rules.

local sinks = {}
local current = ""
local index = 1

local function short(name)
  if name == nil then
    return "audio"
  end
  if #name > 22 then
    return name:sub(1, 21) .. "…"
  end
  return name
end

local function refresh()
  if not noctalia.commandExists("better-audio") then
    barWidget.setText("audio?")
    return
  end
  noctalia.runAsync({ "better-audio", "sinks" }, function(result)
    local value = noctalia.json.decode(result.stdout)
    sinks = {}
    if type(value) == "table" then
      current = value.default or ""
      local list = value.sinks or {}
      for _, sink in ipairs(list) do
        if type(sink) == "table" and type(sink.name) == "string" then
          table.insert(sinks, sink.name)
        end
      end
      for i, name in ipairs(sinks) do
        if name == current then
          index = i
        end
      end
    end
    barWidget.setText(short(current))
    barWidget.setColor("primary", "script")
  end)
end

local function set_default(name)
  noctalia.runAsync({ "better-audio", "default", name }, function(result)
    if result.exitCode ~= 0 then
      noctalia.notifyError("Better Audio", result.stderr or ("could not switch to " .. name))
    else
      current = name
      barWidget.setText(short(current))
      noctalia.notify("Better Audio", "default sink: " .. name)
    end
  end)
end

function update()
  noctalia.setUpdateInterval(15000)
  barWidget.setGlyph("volume")
  barWidget.setTooltip("better-audio default sink")
  refresh()
end

function onClick()
  if #sinks == 0 then
    refresh()
    return
  end
  index = (index % #sinks) + 1
  set_default(sinks[index])
end

function onScroll(_axis, steps, startsGesture)
  if not startsGesture or #sinks == 0 then
    return
  end
  index = ((index - 1 + steps) % #sinks) + 1
  set_default(sinks[index])
end

function onRightClick()
  noctalia.runAsync({ "better-audio", "apply" }, function(result)
    if result.exitCode ~= 0 then
      noctalia.notifyError("Better Audio", result.stderr or "apply failed")
    else
      noctalia.notify("Better Audio", "routing rules applied")
      refresh()
    end
  end)
end

function onIpc(event)
  if event == "refresh" then
    refresh()
  end
end
