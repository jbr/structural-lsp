local root = vim.fn.fnamemodify('../nvim-lspconfig', ':p')
vim.opt.rtp:prepend(root)
local files = vim.fn.glob(root .. 'lsp/*.lua', false, true)
local out = {}
local function src(fn, path)
  local info = debug.getinfo(fn, 'S')
  if not info or info.linedefined <= 0 then return nil end
  local lines = vim.fn.readfile(path)
  return table.concat(vim.list_slice(lines, info.linedefined, info.lastlinedefined), '\n')
end
for _, f in ipairs(files) do
  local name = vim.fn.fnamemodify(f, ':t:r')
  local ok, cfg = pcall(dofile, f)
  local r = { server_id = name, file = 'lsp/' .. name .. '.lua' }
  if not ok then r.load_error = tostring(cfg) else
    local c = cfg.cmd
    if type(c) == 'table' then r.command = c elseif type(c) == 'function' then r.command_fn_source = src(c, f) end
    r.filetypes = cfg.filetypes
    r.root_markers = cfg.root_markers
    if type(cfg.root_dir) == 'function' then r.root_dir_fn_source = src(cfg.root_dir, f)
    elseif type(cfg.root_dir) == 'string' then r.root_dir_static = cfg.root_dir end
    r.workspace_required = cfg.workspace_required
    r.single_file_support = cfg.single_file_support
    local lines = vim.fn.readfile(f)
    local head = {}
    for _, l in ipairs(lines) do
      if l:match('^%-%-%-') then table.insert(head, (l:gsub('^%-%-%- ?', ''))) elseif #head > 0 then break end
    end
    local doc = table.concat(head, '\n')
    r.homepage = doc:match('https?://[^%s%)>]+')
    r.doc = doc:sub(1, 1500)
  end
  table.insert(out, r)
end
vim.fn.writefile({ vim.json.encode(out) }, 'out/nvim-lspconfig.raw.json')
print(#out)
