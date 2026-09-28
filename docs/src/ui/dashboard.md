# Dashboard

The start page after sign-in: every task that needs attention, across all projects, in one list.

<!-- screenshot: dashboard with stats, filter, task list and rail -->

| Area | Shows | Actions |
|---|---|---|
| Stats | CPU time, builds, cache size, busy workers and queue wait across the visible projects | |
| Filter | **All**, **Failing** and **Starred**, each with a count | Switch the task list; the choice stays in the URL |
| Task list | The top 10 tasks: latest commit, recent evaluations as bars, entry points ok/total, change in failures, speed | Open a task from the row, an evaluation from a bar; **Show all** pages through every task |
| Activity | A calendar of the last year | Switch between evaluations and failures |
| Rail | Projects by rank, caches with starred first | Star a project or cache; open its tasks |

Bars in the task list: height is the duration, color the status.

New users without projects or caches see the first steps instead: create a project, add a task, connect a worker, create or subscribe to a cache.

## Ranking

The task list holds the tasks of the user's projects, plus starred tasks of public projects, in four tiers:

1. Starred and active
2. Active
3. Starred
4. Member only

Active means an evaluation other than a pull request in the last 14 days. Within a tier, the newest evaluation comes first. Pull request evaluations count toward the stats only.

## Stars

- Star a project, task or cache from the page header, or a project or cache from the rail.
- Stars are personal: they rank and filter only the user's own view.
- Starred items come first in the task list, the rail and the command palette.

## Command Palette

`/` anywhere outside a text field, or the search field in the header.

| Query | Finds |
|---|---|
| A name | Projects, tasks and caches, starred first |
| 7 to 40 hex characters | Commits by hash prefix |
| A store path or NAR hash | NARs in every readable cache |
| Empty | The starred items |

Arrow keys or Ctrl+J / Ctrl+K move, Enter opens, Escape closes.

## Related

- [Projects and Tasks](../concepts/projects-and-tasks.md): what the task list shows
- [Evaluations and Builds](../concepts/evaluations-and-builds.md): the statuses behind the bars
