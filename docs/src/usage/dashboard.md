<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>

SPDX-License-Identifier: AGPL-3.0-only
-->

# Dashboard

The dashboard routes you to what needs attention. Its sections follow your data (projects, caches, superuser or worker owner) and are never configured.

## Page

- **Stats line**: CPU time, builds, cache size, workers busy and queue wait across your projects and public projects (all projects for superusers).
- **Search**: opens the [command palette](#command-palette).
- **Chips**: All, Failing, Got worse, Starred, each with its count over all of your tasks. The selected chip is kept in the URL (`?filter=`).
    - Failing: the latest evaluation failed or has failing entry points.
    - Got worse: more failing entry points than the previous evaluation.
- **Task table**: the top 10 tasks. "Show all" lists every task, 25 per page. Each row shows the star, `project / task` with the latest commit and its age, the last evaluations, entry points ok/total, the delta against the previous evaluation, speed, reliability and evaluations per week.
    - Bars show the last evaluations: height is the duration, color the status, hover for details. The number of bars follows the available width.
    - A row opens the task, a bar opens its evaluation.
- **Activity**: a calendar of the last 371 days, switchable between evaluations and failures.
- **Rail**: your projects, ordered by tier (see [Ranking](#ranking)), and your caches, starred first. Starred and active projects list their tasks. Superusers and worker owners also get Operations links (Job Board, Workers, Scheduler, Health).
- **New users** (no projects, no caches) only see the first steps: create a project, add a task, connect a worker, create or subscribe to a cache.
- Each block loads on its own. A failed block shows an inline retry, a block you may not read is hidden.

## Ranking

- The task table covers the tasks of projects you are a member of, plus tasks you starred in public projects.
- A task star ranks the task in the table, a project star ranks the project in the rail.
- Active means a non-PR evaluation in the last 14 days.
- Tiers, in order:
    1. starred and active
    2. active
    3. starred (also a starred task in a public project you are not a member of)
    4. member only
- Within a tier the newest latest evaluation comes first.
- Pull request evaluations are excluded everywhere on the dashboard: stats, history bars, deltas, activity and "active".

## Stars

- Star a project, task or cache from its page header, the task table or the rail.
- Stars are personal: they only rank and filter what you see. Task stars drive the task table and the Starred chip, project and cache stars order the rail, and all of them come first in palette results.
- Stars are shown to signed-in users only.
- A star on something you can no longer read is dropped from every list.

## Command palette

- Open it with `/` on any page outside a text field, or through the search field on the dashboard.
- Move through the results with the arrow keys or Ctrl+J / Ctrl+K, open one with Enter, close with Escape.
- It finds:
    - projects, tasks and caches by name (case-insensitive substring, starred first)
    - commits by hash prefix (7 to 40 hex characters); names matching the same text are still listed
    - NARs in every cache you can read, by store path or 32-character NAR hash
- With an empty query it lists your starred items.
