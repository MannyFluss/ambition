-- Ambition's press in Neovim (0.10 or later): control-option-return sends the
-- line under the cursor. Nothing is saved: Ambition reads the buffer through
-- its language server (below) and puts the answer back in as an edit, which
-- `u` undoes. Put this file in ~/.config/nvim/plugin/, or paste it into your
-- init.lua.
--
-- A terminal Neovim sees <C-M-CR> only if the terminal sends it (kitty,
-- WezTerm and Ghostty do; Terminal.app does not). If it does not, map
-- another key the same way, such as <leader><CR>.

local function press()
  local file = vim.api.nvim_buf_get_name(0)
  if file == '' then
    return vim.notify('Ambition: this buffer needs a file name first', vim.log.levels.WARN)
  end
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
vim.api.nvim_create_user_command('AmbitionPress', press, { desc = 'Ambition: press on the line under the cursor' })

-- The language server: Ambition's way into the unsaved buffer, and colors,
-- hover and completion on caret lines.
if vim.lsp.config then
  vim.lsp.config('ambition', { cmd = { 'ambition', 'lsp' },
    filetypes = { 'markdown', 'text', 'python', 'javascript', 'typescript', 'rust', 'lua', 'sh' } })
  vim.lsp.enable('ambition')
end
