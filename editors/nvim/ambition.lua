-- Ambition's press in Neovim (0.10 or later): control-option-return saves the
-- file, then sends the ^ line under the cursor. Put this file in
-- ~/.config/nvim/plugin/, or paste it into your init.lua.
--
-- A terminal Neovim sees <C-M-CR> only if the terminal sends it (kitty,
-- WezTerm and Ghostty do; Terminal.app does not). If it does not, map
-- another key the same way, such as <leader><CR>.

local function press()
  local file = vim.api.nvim_buf_get_name(0)
  if file == '' then
    return vim.notify('Ambition: save this buffer to a file first', vim.log.levels.WARN)
  end
  vim.cmd('silent update')
  local where = file .. ':' .. vim.api.nvim_win_get_cursor(0)[1]
  vim.system({ 'ambition', 'fire', where }, { text = true }, function(r)
    if r.code ~= 0 then
      vim.schedule(function()
        vim.notify(vim.trim(r.stderr ~= '' and r.stderr or r.stdout), vim.log.levels.ERROR)
      end)
    end
  end)
end

vim.keymap.set({ 'n', 'i' }, '<C-M-CR>', press, { desc = 'Ambition: press' })
vim.api.nvim_create_user_command('AmbitionPress', press, { desc = 'Ambition: send the ^ line under the cursor' })

-- The language server, for colors, hover and completion on caret lines.
if vim.lsp.config then
  vim.lsp.config('ambition', { cmd = { 'ambition', 'lsp' },
    filetypes = { 'markdown', 'text', 'python', 'javascript', 'typescript', 'rust', 'lua', 'sh' } })
  vim.lsp.enable('ambition')
end
