local d = vim.json.decode(table.concat(vim.fn.readfile('cache/nvim-filetypes.json'), '\n'))
local n = 0
for ext, v in pairs(d.extension) do
  if v == '<function>' then
    local ok, r = pcall(vim.filetype.match, { filename = '/tmp/example.' .. ext })
    d.extension[ext] = (ok and r) and (r .. ' (dynamic)') or '<content-dependent>'
    n = n + 1
  end
end
vim.fn.writefile({ vim.json.encode(d) }, 'cache/nvim-filetypes.json')
print(n)
