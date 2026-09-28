# Overview

## Evaluation Wildcard

See [Evaluation Wildcards](../reference/wildcards.md).

## Evaluations

Click **Start Evaluation** on the task page. Gradient clones the repo, evaluates each wildcard match, and dispatches the resulting derivations to connected workers.

An evaluation skips every dependency subtree Gradient has already recorded. If a recorded graph went wrong, **Full rewalk** in the evaluation menu starts an evaluation that walks the whole closure again and fills in whatever the record is missing.

The evaluation log page shows per-build status, combined ANSI build output, and an **Abort** button.
Builds are grouped by status, and within a group listed in dependency order - the entry point first,
then each dependency layer sorted by name - so a build appears above the builds it needs. Clicking a
section header collapses it; arrow keys then skip its builds.

Right-clicking a build in the list opens its actions: **Graph** (the dependency graph), **Show Job**
(the Job Board entry for the dispatch that ran it), **Artefacts**, and **Download Log**, which saves
the complete log rather than the portion currently on screen.

Evaluations can also be triggered automatically:

- **GitHub App** - when the App is installed, push events from GitHub trigger evaluations instantly (no polling). See [GitHub App](../guides/forge-github.md).
- **Forge webhooks** - for Gitea, Forgejo, GitLab, or GitHub without the App, configure a per-project push webhook. See [Forge Webhooks](../guides/forge-gitea.md).
- **Polling** - fallback for tasks without webhook configuration; Gradient checks for new commits every 60 seconds.

## Members & Roles

Each project manages its own members and roles under
**Project → Settings → Members & Roles**.

### Built-in roles

Every project carries the same three immutable system roles:

| Role  | What it can do                                                                                            |
|-------|-----------------------------------------------------------------------------------------------------------|
| Admin | Everything: full settings, member & role management, task lifecycle, project deletion.           |
| Write | Create/edit tasks, manage webhooks/integrations/workers/cache subscriptions, trigger evaluations.     |
| View  | Read members-only content; mutate non-secret sub-resources only (workers, integrations, cache subs, SSH key). |

Built-in roles cannot be edited or deleted.

### Custom roles

Members holding the `manageRoles` capability can create project-specific custom
roles by ticking individual permissions:

- Project-level: `viewProject`, `manageProjectSettings`, `deleteProject`, `manageMembers`,
  `manageRoles`, `manageIntegrations`, `manageWebhooks`, `manageWorkers`,
  `manageSubscriptions`, `manageSshKey`.
- Task-level: `createTask`, `editTask`, `triggerEvaluation`.

Permission identifiers and their canonical order are returned by
`GET /api/v1/projects/{project}/roles`'s `available_permissions` field -
new capabilities are appended over time and the UI picks them up
automatically.

A role currently assigned to one or more members cannot be deleted; reassign
the affected members first.

## Appearance

**Settings -> Profile -> Appearance** picks the colour theme: **System**,
**Light** or **Dark**. It applies as soon as you choose it.

System follows the operating system's `prefers-color-scheme` setting and keeps
following it, so a machine that switches to dark at sunset switches Gradient
with it. Light and Dark pin the choice regardless of the OS.

The preference lives in the browser's local storage rather than on your account,
so it is per browser: a second device starts on System until it is set there
too. Clearing site data returns it to System.

## SSH Keys

Each project has one Ed25519 SSH key pair, generated automatically. The public key is shown in **Project → Settings → SSH**.

Add this key to your **Git hosts** as a deploy key so Gradient can clone private repositories.

The key is scoped to the project; different projects use different keys.

## Workers

Build capacity is provided by `gradient-worker` processes. The server does not start a worker automatically - at least one must be configured explicitly.

To run a worker on the server host itself, enable `services.gradient.worker`. Workers authenticate using per-project tokens. A worker authorized for a project receives only that project's job offers.
