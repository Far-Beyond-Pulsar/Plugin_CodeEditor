-- Timers: closures, tables and a simple update loop.

local Timers = {}
Timers.__index = Timers

-- Makes an empty timer list.
function Timers.new()
  return setmetatable({ active = {}, now = 0 }, Timers)
end

-- Calls `callback` after `delay` seconds; with `repeating`, every `delay` seconds.
-- Returns a function that cancels the timer.
function Timers:after(delay, callback, repeating)
  local timer = { due = self.now + delay, delay = delay, callback = callback, repeating = repeating }
  table.insert(self.active, timer)
  return function() timer.cancelled = true end
end

-- Advances time by `dt` seconds and runs every timer that is due.
function Timers:update(dt)
  self.now = self.now + dt
  local keep = {}
  for _, timer in ipairs(self.active) do
    if not timer.cancelled then
      if self.now >= timer.due then
        timer.callback()
        if timer.repeating then
          timer.due = timer.due + timer.delay
          table.insert(keep, timer)
        end
      else
        table.insert(keep, timer)
      end
    end
  end
  self.active = keep
end

local timers = Timers.new()
timers:after(1.0, function() print("one second") end)
local cancel = timers:after(0.5, function() print("tick") end, true)

for _ = 1, 8 do
  timers:update(0.25)
end
cancel()
