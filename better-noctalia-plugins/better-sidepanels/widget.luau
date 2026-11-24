--!nonstrict
-- Bar widget: validate the better-sidepanels config with left click and send
-- SIGHUP (reload) with right click. Scroll also reloads.

local function validate()
  if not noctalia.commandExists("better-sidepanels") then
    noctalia.notifyError("Better Sidepanels", "better-sidepanels is not installed")
    return
  end
  noctalia.runAsync({ "better-sidepanels", "--validate" }, function(result)
    if result.exitCode == 0 then
      noctalia.notify("Better Sidepanels", "configuration is valid")
      barWidget.setColor("primary", "script")
    else
      noctalia.notifyError("Better Sidepanels", result.stderr or "invalid configuration")
      barWidget.setColor("error", "script")
    end
  end)
end

local function reload()
  noctalia.runAsync({ "pkill", "-HUP", "-x", "better-sidepanels" }, function(result)
    if result.exitCode ~= 0 then
      noctalia.notifyError("Better Sidepanels", "no running better-sidepanels")
    else
      noctalia.notify("Better Sidepanels", "reloaded")
    end
  end)
end

function update()
  noctalia.setUpdateInterval(60000)
  barWidget.setGlyph("layout-sidebar")
  barWidget.setText("")
  barWidget.setTooltip("better-sidepanels: click validates, right click reloads")
end

function onClick()
  validate()
end

function onRightClick()
  reload()
end

function onScroll(_axis, _steps, startsGesture)
  if startsGesture then
    reload()
  end
end
