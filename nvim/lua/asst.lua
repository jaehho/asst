-- asst in Neovim. `:AsstTask` opens the quick-add popup. It all goes through
-- the `asst` CLI, so asstd has to be running.
local M = {}

local function say(msg, level)
  vim.notify('asst: ' .. msg, level or vim.log.levels.INFO)
end

-- A new task, typed into the quick-add popup.
function M.new_task()
  local ok, err = pcall(vim.system, { 'asst-gtk', 'quick-add' }, { detach = true })
  if not ok then
    say('asst-gtk is not installed: ' .. tostring(err), vim.log.levels.ERROR)
  end
end

return M
