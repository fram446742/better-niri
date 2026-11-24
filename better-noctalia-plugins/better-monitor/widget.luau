--!nonstrict
-- Bar widget: cycle saved better-monitor output profiles.
-- Left click applies the next profile, right click applies `default`.

local profiles = {}
local index = 0

local function decode(raw)
  if raw == nil or raw == "" then
    return nil
  end
  local value = noctalia.json.decode(raw)
  return value
end

local function profile_names(value)
  if type(value) ~= "table" then
    return {}
  end
  local data = value.data or value
  local list = data.profiles or data
  local names = {}
  if type(list) == "table" then
    for _, name in ipairs(list) do
      if type(name) == "string" then
        table.insert(names, name)
      end
    end
  end
  return names
end

local function refresh()
  if not noctalia.commandExists("better-monitor") then
    barWidget.setText("display?")
    return
  end
  noctalia.runAsync({ "better-monitor", "list", "--json" }, function(result)
    local value = decode(result.stdout)
    profiles = profile_names(value)
    if #profiles == 0 then
      barWidget.setText("display")
      barWidget.setColor("on_surface", "script")
    else
      barWidget.setText("display " .. tostring(#profiles))
      barWidget.setColor("primary", "script")
    end
  end)
end

local function apply(name)
  if name == nil then
    noctalia.notifyError("Better Monitor", "no saved profiles")
    return
  end
  noctalia.runAsync({ "better-monitor", "apply", name }, function(result)
    if result.exitCode ~= 0 then
      noctalia.notifyError("Better Monitor", result.stderr or ("could not apply " .. name))
    else
      noctalia.notify("Better Monitor", "applied " .. name)
      refresh()
    end
  end)
end

function update()
  noctalia.setUpdateInterval(30000)
  barWidget.setGlyph("device-desktop")
  barWidget.setTooltip("better-monitor profiles")
  refresh()
end

function onClick()
  if #profiles == 0 then
    refresh()
    return
  end
  index = (index % #profiles) + 1
  apply(profiles[index])
end

function onRightClick()
  apply("default")
end

function onScroll(_axis, steps, startsGesture)
  if not startsGesture or #profiles == 0 then
    return
  end
  index = ((index - 1 + steps) % #profiles) + 1
  apply(profiles[index])
end

function onIpc(event)
  if event == "refresh" then
    refresh()
  end
end
