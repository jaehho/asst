-- The asst command (lua/asst.lua).
if vim.g.loaded_asst then
  return
end
vim.g.loaded_asst = true

vim.api.nvim_create_user_command('AsstTask', function()
  require('asst').new_task()
end, { desc = 'asst: a new task in the quick-add popup' })
