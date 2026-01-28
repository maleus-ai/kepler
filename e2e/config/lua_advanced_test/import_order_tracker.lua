-- This file is loaded via lua_import: and should run AFTER lua: block
-- (lua: block is processed first in YAML order)
table.insert(global.load_order, "lua_import")
