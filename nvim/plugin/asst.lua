-- Commands for notes linked to tasks (lua/asst.lua).
if vim.g.loaded_asst then
  return
end
vim.g.loaded_asst = true

for name, how in pairs {
  AsstTask = { 'new_task', 'asst: a new task linked to this note' },
  AsstAttach = { 'attach', 'asst: link this note to an open task' },
  AsstTasks = { 'tasks', 'asst: the open tasks linking this note' },
} do
  vim.api.nvim_create_user_command(name, function()
    require('asst')[how[1]]()
  end, { desc = how[2] })
end
