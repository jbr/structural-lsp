local ft = vim.filetype
local tables = {}
local function grab(fn)
  local i = 1
  while true do
    local name, val = debug.getupvalue(fn, i)
    if not name then break end
    if (name == 'extension' or name == 'filename' or name == 'pattern') and type(val) == 'table' then tables[name] = val end
    if type(val) == 'function' and not tables._seen then end
    i = i + 1
  end
end
grab(ft.match)
-- also try add() which references the same tables
grab(ft.add)
local out = { extension = {}, filename = {}, pattern = {} }
for kind, t in pairs(tables) do
  for k, v in pairs(t) do
    if type(v) == 'string' then out[kind][k] = v
    elseif type(v) == 'table' and type(v[1]) == 'string' then out[kind][k] = v[1]
    elseif kind == 'pattern' and type(v) == 'table' then
      for pk, pv in pairs(v) do
        if type(pv) == 'string' then out.pattern[pk] = pv elseif type(pv) == 'table' and type(pv[1]) == 'string' then out.pattern[pk] = pv[1] else out.pattern[pk] = '<function>' end
      end
    else out[kind][k] = '<function>' end
  end
end
vim.fn.writefile({ vim.json.encode(out) }, 'cache/nvim-filetypes.json')
print(vim.tbl_count(out.extension), vim.tbl_count(out.filename), vim.tbl_count(out.pattern))
