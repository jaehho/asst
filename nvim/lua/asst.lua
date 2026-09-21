-- asst in Neovim, for notes that tasks link to. `:AsstTask` adds a task
-- linked to the note being edited (the quick-add popup), `:AsstAttach` links
-- the note to an open task, and `:AsstTasks` lists the open tasks linking it
-- and shows the one picked in the window. It all goes through the `asst` CLI,
-- so asstd has to be running, and the note has to be in its notes folder.
local M = {}

local function say(msg, level)
  vim.notify('asst: ' .. msg, level or vim.log.levels.INFO)
end

-- The note being edited, written first so the file is there to link.
local function note()
  local path = vim.api.nvim_buf_get_name(0)
  if path == '' then
    say('write the note to a file first', vim.log.levels.WARN)
    return nil
  end
  if vim.bo.modified then
    vim.cmd.write()
  end
  return path
end

-- The CLI's answer as JSON (true when it printed none), or nil once the
-- problem has been shown.
local function asst(args)
  local ok, result = pcall(function()
    return vim.system(vim.list_extend({ 'asst', '--json' }, args), { text = true }):wait()
  end)
  if not ok then
    say('the asst command is not installed', vim.log.levels.ERROR)
    return nil
  end
  if result.code ~= 0 then
    say((vim.trim(result.stderr or ''):gsub('^asst: ', '')), vim.log.levels.ERROR)
    return nil
  end
  if vim.trim(result.stdout or '') == '' then
    return true
  end
  local decoded, value = pcall(vim.json.decode, result.stdout)
  return decoded and value or true
end

local function label(task)
  return task.summary .. '  ·  ' .. task.list_name
end

-- Pick one of `tasks`: in Telescope when it is installed, else vim.ui.select.
function M.pick(tasks, prompt, on_choice)
  local has_telescope, pickers = pcall(require, 'telescope.pickers')
  if not has_telescope then
    vim.ui.select(tasks, { prompt = prompt, format_item = label }, function(task)
      if task then
        on_choice(task)
      end
    end)
    return
  end
  local actions = require 'telescope.actions'
  local state = require 'telescope.actions.state'
  pickers
    .new(require('telescope.themes').get_dropdown {}, {
      prompt_title = prompt,
      finder = require('telescope.finders').new_table {
        results = tasks,
        entry_maker = function(task)
          return { value = task, display = label(task), ordinal = label(task) }
        end,
      },
      sorter = require('telescope.config').values.generic_sorter {},
      attach_mappings = function(buffer)
        actions.select_default:replace(function()
          local entry = state.get_selected_entry()
          actions.close(buffer)
          if entry then
            on_choice(entry.value)
          end
        end)
        return true
      end,
    })
    :find()
end

-- A new task linked to this note, typed into quick add.
function M.new_task()
  local path = note()
  if not path then
    return
  end
  local ok, err = pcall(vim.system, { 'asst-gtk', 'quick-add', '--attach', path }, { detach = true })
  if not ok then
    say('asst-gtk is not installed: ' .. tostring(err), vim.log.levels.ERROR)
  end
end

-- Link this note to an open task.
function M.attach()
  local path = note()
  if not path then
    return
  end
  local tasks = asst { 'ls' }
  if type(tasks) ~= 'table' then
    return
  end
  if #tasks == 0 then
    say 'there are no open tasks'
    return
  end
  M.pick(tasks, 'Link this note to', function(task)
    if asst { 'attach', task.id, path } then
      say('linked to “' .. task.summary .. '”')
    end
  end)
end

-- The open tasks linking this note; the one picked opens in the window.
function M.tasks()
  local path = note()
  if not path then
    return
  end
  local tasks = asst { 'ls', '--attached', path }
  if type(tasks) ~= 'table' then
    return
  end
  if #tasks == 0 then
    say 'no open task links this note'
    return
  end
  M.pick(tasks, 'Tasks linking this note', function(task)
    asst { 'open', task.id }
  end)
end

return M
