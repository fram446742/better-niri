--!nonstrict
-- Bar widget: validate the loom-sidepanels config with left click and send
-- SIGHUP (reload) with right click. Scroll also reloads.

local function validate()
  if not noctalia.commandExists("loom-sidepanels") then
    noctalia.notifyError("Loom Sidepanels", "loom-sidepanels is not installed")
    return
  end
  noctalia.runAsync({ "loom-sidepanels", "--validate" }, function(result)
    if result.exitCode == 0 then
      noctalia.notify("Loom Sidepanels", "configuration is valid")
      barWidget.setColor("primary", "script")
    else
      noctalia.notifyError("Loom Sidepanels", result.stderr or "invalid configuration")
      barWidget.setColor("error", "script")
    end
  end)
end

local function reload()
  noctalia.runAsync({ "pkill", "-HUP", "-x", "loom-sidepanels" }, function(result)
    if result.exitCode ~= 0 then
      noctalia.notifyError("Loom Sidepanels", "no running loom-sidepanels")
    else
      noctalia.notify("Loom Sidepanels", "reloaded")
    end
  end)
end

function update()
  noctalia.setUpdateInterval(60000)
  barWidget.setGlyph("layout-sidebar")
  barWidget.setText("")
  barWidget.setTooltip("loom-sidepanels: click validates, right click reloads")
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

function onIpc(event)
  if event == "refresh" then
    refresh()
  elseif event == "click" then
    onClick()
  end
end
