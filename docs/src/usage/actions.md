# Actions

Actions are the response-side counterpart of Triggers. Where a Trigger fires an evaluation when an event arrives, an Action reacts to evaluation and build lifecycle events and does something: sends an email, calls a webhook, or posts a commit status back to a forge.

Actions are per-project. Four types ship in v1:

| Type | Summary | Prerequisite |
|---|---|---|
| `send_mail` | Email one or more recipients | Server SMTP configured |
| `send_web_request` | HTTP POST to an external URL | None |
| `forge_status_report` | Post commit status to a forge | Outbound integration in the org |
| `open_pr` | Open/update a flake.lock-update pull request | Outbound integration in the org |

## Events

| Event | Fired when |
|---|---|
| `evaluation.queued` | Evaluation enters the queue |
| `evaluation.started` | Evaluation begins running |
| `evaluation.building` | Evaluation enters the building phase |
| `evaluation.completed` | Evaluation completed successfully |
| `evaluation.failed` | Evaluation failed |
| `evaluation.aborted` | Evaluation was aborted |
| `evaluation.action_required` | Evaluation parked waiting for maintainer approval on a fork PR |
| `evaluation.approval_granted` | Maintainer cleared the approval gate (flips the `Approval` check to success) |
| `build.queued` | Build enters the queue |
| `build.started` | Build starts executing on a worker |
| `build.completed` | Build completed successfully |
| `build.failed` | Build failed |
| `build.substituted` | Build output came from an upstream cache substitution |

An action with an empty `events` list never fires. `forge_status_report` ignores the `events` list - it is hard-wired to `build.started`, `build.completed`, and `build.failed`.

## Send Mail

Uses the server-global SMTP configuration (`services.gradient.email.*`). If SMTP is not configured, creating a `send_mail` action returns `400`.

**Config fields:**

| Field | Required | Description |
|---|---|---|
| `recipients` | yes | List of email addresses |
| `subject_template` | no | Subject line with placeholders |

**Subject placeholders:** `{event}`, `{project}`, `{org}`, `{id}`, `{status}`

Default subject: `[Gradient] {event}: {project}`

Default body includes: event name, project slug, entity id (eval/build UUID), status, and a link to the Gradient UI.

## Send Web Request

POSTs a JSON payload to a URL. Optional `Authorization: Bearer <token>` header.

**Config fields:**

| Field | Required | Description |
|---|---|---|
| `url` | yes | HTTPS endpoint |
| `token` | no | Bearer token (write-only; never returned in reads) |

**Request headers:**

```http
Content-Type: application/json
X-Gradient-Event: build.completed
Authorization: Bearer <token>   # only if token is set
```

**Payload shape:**

```json
{
  "event": "build.completed",
  "project": "my-project",
  "organization": "acme",
  "id": "<eval-or-build-uuid>",
  "status": "completed"
}
```

Token management: the plaintext token is revealed exactly once - on create or after `POST .../regenerate-token`. Store it immediately.

## Forge Status Report

Posts commit status (pending / success / failure / action-required) back to the forge as three separate check runs per PR - `gradient/{project}: Approval` (fork-PR gate), `gradient/{project}: Evaluation` (eval phase), and `gradient/{project}: Build {label}` (one per build, labelled by entry-point name or by `{drv-name}.{architecture}` when no entry-point matched). Each check is updated in place as the phase progresses; the Approval check flips to Success when a maintainer clears the gate, and the Evaluation check is posted as Pending at the same instant so the PR immediately reflects that the pipeline is in flight.

A run that targets a wildcard other than the project default - e.g. `/gradient run packages.x86_64-linux.foo` - reports under `gradient/{project}: Evaluation: {wildcard}` so the custom run shows as its own check line instead of overwriting the default evaluation check.

**Maintainer-initiated runs skip the fork-PR approval gate.** The gate only exists to hold untrusted external contributions; when the action comes from a repo writer it is not needed. The Evaluation runs immediately (no `Approval` check) when any of these happen: a maintainer issues `/gradient run` / `/gradient approve` on the PR, a maintainer submits an approving review through the forge's native PR-review UI (GitHub / Gitea / Forgejo `pull_request_review`), or a maintainer force-pushes onto the contributor's branch. In every case the actor is verified as a repo writer via the forge API before the gate is cleared. GitLab is the exception - it emits no webhook on merge-request approval, so use `/gradient approve` there.

**Config fields:**

| Field | Required | Description |
|---|---|---|
| `integration_id` | yes | UUID of an outbound integration in the same org |

The integration must be `kind: outbound`. The forge type determines the API call format (Gitea, GitLab, GitHub App).

## Open PR

Opens (or updates) a pull request that bumps the project's flake inputs, driven by a native `flake.lock` updater. Unlike the other actions it does not react to a project's ordinary runs: it fires on a verify-gate event (default `build.completed`) but only for `input_update` evaluations.

**Config fields:**

| Field | Required | Default | Description |
|---|---|---|---|
| `integration_id` | yes | (none) | UUID of an outbound integration in the same org |
| `generator` | no | `flake_lock` | Update generator; only the native flake.lock updater exists in v1 |
| `granularity` | no | `per_run` | `per_run` opens one PR for the whole run; `per_input` opens one PR per tracked input |
| `verify_gate` | no | `build` | How far the candidate lock is verified before the PR opens: `none`, `eval`, or `build` |
| `branch_pattern` | no | `gradient/flake-lock-update` | Branch name for the PR; for `per_input` it must contain the `{input}` placeholder |
| `title_template` | no | (none) | PR title with placeholders |
| `body_template` | no | (none) | PR body with placeholders |
| `update_existing` | no | `true` | Update an already-open PR in place instead of opening a duplicate |

**Tracked inputs.** The set of inputs to bump is declared with flake-input override rows on the project. An override whose `url` is unset marks that input as *tracked*, and it will be bumped to its newest revision. As a safety gate, the presence of *any* override with a `url` set (a pinned input) blocks the run, so a project cannot accidentally open a PR while an input is being held at a fixed revision. v1 supports `github`, `gitlab`, and `git` flake inputs.

**PR lifecycle.** A trigger fires; if the project has an `open_pr` action and at least one tracked input, Gradient creates an `input_update` evaluation. The worker bumps each tracked input to its newest revision with a natively recomputed `narHash`, and the candidate lock is verified by a normal eval/build per `verify_gate`. Once the verify gate passes, the PR is opened or, when `update_existing` is true, updated in place. An empty or no-change patch opens no PR.

## Declarative configuration via Nix

```nix
services.gradient.state.projects.web-app = {
  actions = [
    {
      name = "notify-failures";
      type = "send_mail";
      events = [ "evaluation.failed" "build.failed" ];
      config = {
        recipients = [ "ops@example.com" ];
        subject_template = "[Gradient] {event}: {project}";
      };
    }
    {
      name = "webhook-completed";
      type = "send_web_request";
      events = [ "build.completed" ];
      config = {
        url = "https://hooks.example.com/gradient";
        token_file = "/run/credentials/gradient.service/webhook-token";
      };
    }
    {
      name = "github-status";
      type = "forge_status_report";
      config = {
        integration = "github-main";
      };
    }
    {
      name = "flake-lock-pr";
      type = "open_pr";
      config = {
        integration = "github-main";
        granularity = "per_input";
        verify_gate = "build";
        branch_pattern = "gradient/flake-lock-update/{input}";
      };
    }
  ];
};
```

`token_file` is read at activation time and stored encrypted. It is not re-read on reload; rotate with `services.gradient.state.projects.<name>.actions.<n>.config.token_file` and `systemctl restart gradient`.

State-managed actions (`managed: true`) cannot be mutated through the API; remove or change them via NixOS config.

## Troubleshooting

Open the action's **Deliveries** popup in the UI (Actions page → click the delivery count badge on any action row). Each row shows:

- HTTP status or error message
- Duration (ms)
- Request body sent
- Response body received (if any)

Common issues:

| Symptom | Cause |
|---|---|
| `400` on create (send_mail) | SMTP not configured on the server |
| Delivery shows `connection refused` | Target URL unreachable from the server |
| No deliveries logged | Action `active: false`, or no matching events fired |
| `403` on regenerate-token | Action is not of type `send_web_request` |
