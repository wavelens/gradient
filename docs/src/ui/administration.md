<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Administration

Superusers open **Admin** in the header. Operations and lists in this area affect the whole instance.

| Tab | Content | Actions |
|---|---|---|
| Overview | Pending jobs, connected and draining workers, free disk space, running maintenance tasks. Open each tile to view the list. | **Run Deep GC**, **Enable Draining** |
| Workers | Registered workers each have a team or project, connection status, running jobs and drain status. | **Drain**, **Undrain** |
| Storage | Free disk space, stored size for each cache, project and task, evaluations retained for each task and the items that the last deep GC deleted | **Run GC Now**, **Run Deep GC** |
| Git hosts | GitHub App registrations and instance Git host connections. One connection for each GitHub App installation, plus the Gitea, Forgejo, GitLab connections added here. All projects can use them. | **Create on GitHub**, add, edit and delete connections |
| Settings | All server options, their flags, environment variables, values and source. Secret values stay hidden. | None. Change the option in the NixOS module or environment, then restart the server. |

## Draining

Instance draining can block distribution of new jobs and allow running evaluations to finish. A server restart can clear this flag. Stopping without draining is safe. The server can re-queue interrupted work on the next startup.

Start **Drain** on a single worker. That worker can finish the jobs in flight and receive no new jobs. The worker can keep its connection open. Use **Undrain** to end the drain. Team administrators can use the same button on the workers page for their team. A worker can also start a drain after `SIGTERM`. The **Draining** badge can show that state. The UI can omit the **Undrain** button. Worker restarts end the drain.

**Deactivate** can stop a worker right away on the worker pages for a team and a project. The same action can abort running jobs. The confirmation message can suggest a drain first.

## Storage and GC

GC rounds delete evaluations over each task's **Keep Evaluations** limit and cached paths that expire. The rounds run on a timer. **Run GC Now** can start a round at once.

Deep GC passes scan storage for files without a reference from a database row. Those passes delete the files. The **Storage** tab contents show what the last completed deep GC deleted.

Size values come from the cached path of each task's build. Page loads recalculate them. Both tasks count a shared path toward their size.

Open **Settings** for a task and use **Delete old evaluations** in **Danger Zone** to delete old evaluations. Running evaluations and the latest completed evaluation remain.

**Event notifications** stays in the user menu for the time being.

## Related

- [Workers](../concepts/workers.md): Stop and drain workers
- [Caches](../concepts/caches.md#signing-key): Rotate signing keys
- [CLI](../reference/cli.md#administration): Run the same operations from the command line
