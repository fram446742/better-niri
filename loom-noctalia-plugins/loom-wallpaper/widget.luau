--!nonstrict
-- Bar widget: active loom-wallpaper scene. Left click reapplies it,
-- scroll cycles scenes, right click prints the current mapping in a terminal.

local scenes = {}
local active = nil
local index = 1

local function scene_names(value)
  if type(value) ~= "table" then
    return {}
  end
  local list = value.scenes or {}
  local names = {}
  if type(list) == "table" then
    for name in pairs(list) do
      table.insert(names, name)
    end
  end
  table.sort(names)
  return names
end

local function refresh()
  if not noctalia.commandExists("loom-wallpaper") then
    barWidget.setText("wallpaper?")
    return
  end
  noctalia.runAsync({ "loom-wallpaper", "status" }, function(result)
    local value = noctalia.json.decode(result.stdout)
    if type(value) ~= "table" then
      return
    end
    active = value.active
    scenes = scene_names(value)
    if active == nil or active == "" then
      barWidget.setText("wallpaper")
      barWidget.setColor("on_surface", "script")
    else
      barWidget.setText(active)
      barWidget.setColor("primary", "script")
    end
    for i, name in ipairs(scenes) do
      if name == active then
        index = i
      end
    end
  end)
end

local function use(name)
  noctalia.runAsync({ "loom-wallpaper", "scene", "use", name }, function(result)
    if result.exitCode ~= 0 then
      noctalia.notifyError("Loom Wallpaper", result.stderr or ("could not use " .. name))
      return
    end
    noctalia.runAsync({ "loom-wallpaper", "apply" }, function(applied)
      if applied.exitCode ~= 0 then
        noctalia.notifyError("Loom Wallpaper", applied.stderr or "apply failed")
      else
        active = name
        barWidget.setText(name)
        noctalia.notify("Loom Wallpaper", "scene: " .. name)
      end
    end)
  end)
end

function update()
  noctalia.setUpdateInterval(30000)
  barWidget.setGlyph("photo")
  barWidget.setTooltip("loom-wallpaper scene")
  refresh()
end

function onClick()
  if active ~= nil and active ~= "" then
    use(active)
  elseif #scenes > 0 then
    use(scenes[1])
  else
    refresh()
  end
end

function onScroll(_axis, steps, startsGesture)
  if not startsGesture or #scenes == 0 then
    return
  end
  index = ((index - 1 + steps) % #scenes) + 1
  use(scenes[index])
end

function onRightClick()
  noctalia.runInTerminal("loom-wallpaper status")
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
