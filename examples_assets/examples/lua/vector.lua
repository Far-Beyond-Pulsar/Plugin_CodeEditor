-- A 2D vector class with operator metamethods.

local Vec2 = {}
Vec2.__index = Vec2

function Vec2.new(x, y)
  return setmetatable({ x = x or 0, y = y or 0 }, Vec2)
end

-- a + b
Vec2.__add = function(a, b) return Vec2.new(a.x + b.x, a.y + b.y) end
-- a - b
Vec2.__sub = function(a, b) return Vec2.new(a.x - b.x, a.y - b.y) end
-- v * number, or number * v
Vec2.__mul = function(a, b)
  if type(a) == "number" then return Vec2.new(a * b.x, a * b.y) end
  return Vec2.new(a.x * b, a.y * b)
end
-- a == b
Vec2.__eq = function(a, b) return a.x == b.x and a.y == b.y end
-- tostring(v)
Vec2.__tostring = function(v) return string.format("(%.2f, %.2f)", v.x, v.y) end

function Vec2:length()
  return math.sqrt(self.x * self.x + self.y * self.y)
end

-- The unit vector in the same direction (the zero vector stays zero).
function Vec2:normalized()
  local len = self:length()
  if len == 0 then return Vec2.new(0, 0) end
  return self * (1 / len)
end

local velocity = Vec2.new(3, 4)
print(velocity, velocity:length(), velocity:normalized())
print(velocity + Vec2.new(1, 1), 2 * velocity)
