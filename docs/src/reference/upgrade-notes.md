<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Upgrade Notes

The sections for each release cover only the work that admins do after an upgrade, and the changes that users see. New features are in the release notes.

## 2.1

### Fork Pull Requests

- Gradient CI can evaluate fork PRs without maintainer approval. Those evaluations skip project secrets, SSH keys, and HTTPS login for Git and `OpenPr`/`StartEvaluation` actions.
- Approvals cover exactly one head commit. New pushes require a new approval.
- Upgrades stop evaluations that waited for approval before the upgrade. Upgrades create one approval request for each task, pull request, and head commit. A new evaluation can start once maintainers approve that request.
- Gradient does not run evaluations before approval. Newer pushes replace pending approval requests for older commits.
- Gradient can start a fork pull request without approval only when the author is the fork owner and can push to the repository. Maintainer actions on fork pull requests no longer start code from the fork. Those actions include **Update branch** and Reopen.
- GitLab merge requests from forks always wait for approval when opened or updated, even with `require_approval: false`. With `require_approval: false`, a `/gradient run` comment from a member with write permission starts the head commit without project secrets.

### Pull Request Comment Commands

- `/gradient approve <commit>` and `/gradient run <commit>` accept only full 40-character commit hashes.
- Short hashes get a reply with the command that maintainers use for the current head commit.
- Maintainers with a short hash copy the full hash from that reply.
- A `/gradient approve` comment with no commit approves the head commit only when Gradient recorded that head commit at least 60 seconds before the comment.
- All other `/gradient approve` comments without a commit give no approval. Gradient either recorded the head commit later or never recorded the head commit.
- Reply comments from Gradient contain `/gradient approve <commit>` with the full hash of the head commit in that case.
- Behavior of the **Approve and run** button in GitHub checks did not change.
- An approval review on Gitea and Forgejo cleared pending evaluations for the pull request in 2.0.1. The commit in the review did not matter. Now an approval review from a user with write permission approves only the commit of that review in the review API. That commit needs pending approval. A newer change request from the same user, a dismissed review, and a server without a review API give no approval. Comment commands with the full commit hash still approve the commit.

### Trigger Path Filters

- Path filters in `include_paths` and `exclude_paths` accept a total of at most 64 patterns. Each pattern can contain up to 512 characters.
- API requests and state files that exceed those limits fail. Existing triggers with more patterns keep working.
- Fork pull requests that need maintainer approval get an approval check even if path filters skip the commit. Path filters decide which tasks start after approval.
- All Git fetches now fail after 30 minutes or at 16 GiB of received data. Version 2.0.1 didn't have such limits. Fetches by commit hash and the fallback fetch of all branches share those limits. The values stay fixed. Configuration can't change them. First clones of a nixpkgs-sized repository (about 5 GiB) need a connection of about 3 MB/s to finish within the time limit.

### Job Tokens

Job tokens for evaluations without project credentials have limited permissions. These evaluations come from unapproved fork pull requests.

- The gRPC store does not accept these tokens.
- The binary cache does not accept these tokens as passwords.
- These tokens reach only the listed routes. Those routes cover only evaluations that a build request from the same job started.

| Method | Route |
|---|---|
| `POST` | `/evals/{id}/builds` |
| `GET` | `/evals/{id}` |
| `GET` | `/evals/{id}/artefacts` |

All other requests receive `403` with the text below. Normal `gradient build` commands in those jobs continue to work.

```text
job tokens without project credentials for evaluations reach only build requests and the evaluations that those build requests started
```

### Account Handover

A handover is a takeover through `existing_account_id` in the state file, or a claim by the first external sign-in with the same verified email.

- Previous holders lose the password, sessions, SSH keys and the API keys not declared in the state file.
- Previous holders also lose the team memberships from their sign-in group sync.
- Takeovers through the state file also remove the memberships from SCIM, the SCIM external ID and the external sign-in links.
- Memberships from team admins or from the state file stay.
- The first external sign-in of every password account on instances with single sign-on only removes its API keys, sessions and SSH keys.
- Scripts with those API keys stop working.
- Sign-ins in progress during a handover fail as wrong credentials. Users need to sign in again.

### External Sign-In

- The first external sign-in can link an unlinked account only through a verified email equal to the account email.
- Sign-ins that match an account only by username receive the reply below. Users sign in with the password and connect the provider in the user settings.
- Sign-ins fail after a change of the account email or a removal of the link during the sign-in.

```text
The username is taken. Sign in and connect the account from user settings instead.
```

### Team Membership Origins

- Team memberships from the sign-in group sync carry the new origin `sign_in_group` in the member list.
- The origin `group` is now for SCIM memberships only. API clients with a filter on `group` no longer see memberships from sign-in.
- Existing `group` memberships become `sign_in_group` unless SCIM provisioned the user and the team has a SCIM group.

Admins need to review two cases after the upgrade.

- SCIM users in teams with both a SCIM group and a sign-in group keep their `group` memberships. Only SCIM can end these memberships.
- Old SCIM memberships of accounts outside SCIM provisioning appear as `sign_in_group`. Teams without a sign-in group keep these memberships until removal by a team admin.

### State File

- Declared users with the username or email of an account from registration, SCIM or external sign-in are skipped with a logged error.
- `existing_account_id` on the declared user can take over that account.
- Older state users without `password_file` that took over a SCIM account by username count as SCIM accounts after the upgrade.
- These users are skipped. Their accounts keep the superuser flag and the SCIM token can change or delete them. Set `existing_account_id` for these users.
- State export (`GET /admin/state`, `gradient admin state`) changed to include only users declared in the state file. The export is no longer a copy of all accounts.
- Existing projects, tasks, caches and Git host connections with an empty `created_by` keep the stored creator.
- Empty `created_by` values grant no new role to the stored creator.
- New entities with an empty `created_by` fail the apply with `created_by is empty`.

### SCIM

- The SCIM token can reach only accounts from SCIM. Other accounts answer `404`.
- Group `PATCH` requests apply all operations or none.
- `add` and `replace` operations with ids of accounts outside SCIM answer `400` and change nothing.
- `remove` operations skip unknown ids and ids of other accounts.
- User `PUT` requests with the email of another account answer `409`.
- The `userName eq` filter is case-insensitive.

### gRPC Store Limits

- Calls to the gRPC store count toward the rate limit for the caller's network address. Anonymous REST calls use the same limit.
- The code fixes the burst at 150 calls and the refill at 5 calls per second.
- Calls beyond the rate limit fail with gRPC `RESOURCE_EXHAUSTED`.
- Workers and Nix clients behind the same address share one limit.
- Reverse proxies outside `http.trustedProxies` make all callers share the proxy address. Add the proxy ranges to `http.trustedProxies` to enforce limits per client address.
- One API key can have at most 32 calls to the gRPC store open at a time. Calls beyond that limit fail with `RESOURCE_EXHAUSTED`.
- Messages larger than 16 MiB fail with `OUT_OF_RANGE`.
- Calls in progress to the gRPC store end with `PERMISSION_DENIED` after a change to the caller's access rights. Calls can also end when callers gain access rights, for example new cache access. Start a new call. That call can use the new access rights. Calls stop right after the related audit event on the same Gradient server. Calls stop within about 60 seconds at the latest after other changes. Examples include API key revocation, account deactivation, and removal of project access or cache access. Access checks that fail, such as database errors, also end calls.

### Server URL and WebSocket

- `serve_url` needs a full URL with scheme and host, for example `https://ci.example.com`.
- Values without a scheme and host stop the server at startup with `invalid serve_url`.
- WebSocket requests from browsers that carry the session cookie and an `Origin` outside the `serve_url` origin receive `403`.
- Keep the `Origin` header on reverse proxies and development proxies, or rewrite it to the `serve_url` origin.
- Non-browser clients authenticate WebSocket connections with `Authorization: Bearer`.
- Requests to a WebSocket route without a WebSocket upgrade get an access denial (`403` or `404`) before the upgrade error (`400` or `426`).

- Visitors without an account or API key can keep up to 256 WebSocket connections open per client address. IPv6 visitors share one limit per /64 network. Version 2.0.1 had no limit. Use the `http.anonymousWebsocketMaxConnectionsPerIp` setting (`GRADIENT_HTTP_ANONYMOUS_WEBSOCKET_MAX_CONNECTIONS_PER_IP`) to change the limit. WebSocket requests over the limit receive `429`. Reverse proxies that `GRADIENT_HTTP_TRUSTED_PROXIES` does not list treat all visitors as one client address. This limit does not affect signed-in users or API keys.
- The live streams of tasks, evaluations, builds, boards and metrics events each carry a WebSocket Ping every 30 seconds. Streams that receive no data from the client for 60 seconds end. Their slots in the limit above become free. Browsers, the CLI and `websocat` respond to pings without any change.
- Live streams and job terminals close when clients need more than 30 seconds to accept one message.
- Sessions of the cache protocol (`/cache/proto`) close after 120 seconds without data from the client. The 120 seconds begin at the end of the last NAR download.

Open WebSockets close with code `1008` and the reason below after any of these changes.

- Session or API key revocation.
- Disabling a user.
- Removal of access to streams.

Streams close immediately on the related audit event. They close no later than about 60 seconds after the change. Changes by an admin, SCIM, or the state file for another user close that user's streams within about 60 seconds. Reconnect clients after a close.

```text
Access to this stream ended.
```

### Tokens and API Keys

- Tokens in the query string (`?token=`) work only on the routes below.
- Requests return `400` with the token in both the `Authorization` header and query string.

| Method | Route |
|---|---|
| `GET` | `/tasks/{project}/{task}/badge` |
| `GET` | `/tasks/{project}/{task}/entry-point-downloads` |

- Job tokens reach only the task of their own job. Other tasks in the same project return `404`, even for public projects.
- Job tokens can't list tasks in a project.
- API keys that are pinned to a project or a cache enforce that pin on all routes, including the badge and entry point download.
- API keys that are limited to source addresses apply that limit on the same routes.

### Project and Cache Permissions

- Superusers need a project role for Terminals, Run and Retry. Superusers can abort without a project role.
- Legacy API keys and custom project roles that have action permissions but no `ViewProject` receive `404` on actions and job terminals of private projects.
- Downloads from the cache over the proto socket need the `ReadStore` permission.
- Requests to a missing cache name now need credentials, same as requests to a private cache. Requests without valid credentials return `401`. Signed-in users get `404`.
- Cache creators who have no cache role no longer see that cache in listings, the dashboard sidebar, or the tile that shows the cache size.
- Creators still see the cache through a project subscribed to the cache.
- Caches from direct cache roles now count in the tile that shows cache size.

### Member and Role Changes

Callers need every permission of the related roles to change members, invitations, roles and team grants on projects and caches.

- Callers that add or invite a member need every permission of the new role.
- Role changes for a member need every permission of the old role and of the new role.
- To remove a member, callers need every permission of that member's role.
- Create, edit and delete operations on a role require all permissions of the role before and after the change.
- Team grants on projects and caches follow the same rule.
- Superusers get no exception on these routes.
- Refused requests receive `403` with the missing permissions in one of the texts below.
- Existing invitations, roles and grants stay in place even if the items exceed the permissions of their creator. Admins need to review them manually.

```text
Users can grant or remove only project permissions that they have. Missing - "<permission>".
A caller can grant or revoke only cache permissions that belong to that caller. Missing - "<permission>".
```

### API Key Restrictions

- Requests with an API key use only the permissions of that key. The full permissions of the owner don't apply.
- An admin with a key restricted to `manageMembers` cannot add or invite a member with roles that go beyond that key's scope.
- A key without `viewProject` can't grant even `View`.

### Job Terminal

The shell, console, and port forwarding of running jobs require the new project permission `openJobTerminal` ("Job terminal").

- Logins through the SSH gateway need the same permission.
- Roles and keys with `triggerEvaluation` can't open a terminal without `openJobTerminal`.
- The built-in Admin and Write roles get the new permission.
- Custom roles with `triggerEvaluation` get the new permission from the upgrade migration. Members of those roles keep terminal access.
- API keys that hold all project permissions also get the new `openJobTerminal` permission from the migration.
- API keys with a shorter permission list lose terminal access. Key owners add `openJobTerminal` to any key that needs terminal access.
- Roles and API keys from the state file or `services.gradient.state` get their declared `permissions` list back at every start.
- Admins add `openJobTerminal` to each declared role and key that still need terminal access. Do this before the upgrade or right after it.
- Logins to the SSH gateway use the member's SSH keys and project roles. API keys don't grant access to that gateway.
- Admins remove `openJobTerminal` from custom roles for a project so that those roles can start evaluations without terminal access.
- One schema migration. No configuration change.

### Owner Team

- Only Team Admins and superusers can set a team as owner of a project or cache. The same group can do this at creation and at owner change.
- Other team members receive `403` with the text below.
- Callers that move a project to an owner team that has a default role for members need every permission of that role.
- Callers can delete the grants of the old team when they unset or replace the owner team of a project.
- These callers need the Admin role of the old team, superuser permission, or all permissions of the role in that grant.
- Team Admins and superusers receive the exception only for requests without an API key.
- Other callers receive `403`. These calls leave the project unchanged.

```text
Only team managers and instance administrators can set the team as the owner of a project or cache.
```

### Moving a Task

- Callers that move a task require `editTask` in the current project and `createTask` in the target project.
- Task creators without these permissions can no longer move their own tasks.
- A task with a running evaluation or an unfinished exec job can't move. These requests receive `409` with the following text.
- The denial code `TransferNotPermitted` no longer exists. API clients that check this code need an update.
- Both projects receive the `task.move` audit event. Webhooks on both projects receive that event too.

```text
The task has a running evaluation or exec job. Wait for the evaluation or exec job to finish, or cancel it. Then move the task.
```

### Trusted Build Outputs

Trust rules affect only what a cache can sign and serve to outside readers. A build can use any stored copy as input. Trusted outputs come from a worker of the project, or carry a signature from a cache that the project can read.

- Builds of every project use any stored copy of a path in the input closure of the build as a build input. Copies stored only in a cache that the project doesn't read count too.
- A signature in a project cache has no effect on when a dependent build can start.
- Builds don't wait for read access to the cache. The API `WaitingReason` doesn't define a kind for that case.
- Projects and users in one Gradient install trust each other. A flake with a store path can make the build read any stored copy of that path. Copies that exist only in caches that the project doesn't read also count. Caches only limit what outside readers can fetch.
- Three parts make up a signing subscription. These are an active read/write subscription to an active cache, signing enabled on that subscription and a secret signing key in the cache.
- Projects with such a subscription have the server sign their trusted stored inputs into that cache, along with the references of those inputs. Build commits include a signature for the project's own outputs. Dependency outputs of other projects receive signatures from a signature sweep. A stored output, an upstream substitution and a finished evaluation start the signature sweep. Sweeps also start at least every 5 minutes.
- Signature sweeps write a signature for a dependency output into the project's cache. Two cases lead to that write. Either a worker built that copy, or that copy is in a cache that the project can read. That copy can be unsigned in another cache that the project can read. Only signed copies count in the cache for the signature.
- The server never signs a copy into that cache when the project can use the copy only as a build input. Outside readers of that cache get no narinfo for such a dependency output. The cache answers not found.
- The cache can still serve the project's own output. The narinfo for that output can still list dependency outputs in its references.
- Outside readers of the cache get `404` for a dependency output until the sweep signed it. The signature typically follows within seconds of the commit. A full page of 500 waiting builds took about 14 seconds on a large instance.
- A lost wake-up delays the signature until the 5-minute timer of the sweep. A failed sweep page with a single waiting build sets that build aside for 10 minutes. Both causes together give an upper bound of about 19 minutes.
- Set-aside builds exist only in memory on the server. All set-aside periods end on server restart.
- The periodic signing pass does not sign dependency outputs for builds with an evaluation that completed more than 24 hours ago. The server can sign them when later evaluations need them. Admins can start such an evaluation so the server can sign these outputs.
- Subscriptions with signing off write outputs without a signature. Enabling signing later does not add a signature to those outputs. The cache signs that output at the point where a build of a project with a signing subscription needs the output as an input.
- Members of a project see nothing of a cache that the project doesn't read.
- Stored outputs of fixed-output builds from other projects (downloads with a declared hash) follow the same rule as all other stored inputs. They have no special rule.
- Build workers receive unsigned stored inputs only for paths in the input closure of their build. The job's first read request is the start of the server's computation of that closure. These computations use about 100 KB of memory per job. Requests for paths outside the closure trigger a closure recomputation when no usable copy is available. That recomputation can happen at most once per 5 seconds for each job. Paths that remain outside the closure get a "not available" response.
- Periodic consistency checks rebuild completed builds that have no stored copy of a needed output. Passes handle at most 64 outputs, oldest first. A build with two successful attempts can rebuild again only 24 hours after its last success. The server writes a warning to the log on a rebuild of such a build. That warning names the build and the worker of its last successful attempt.
- Periodic checks rebuild a completed build when needed stored outputs point to an unstored path with no pending build. Such an output can point to that path directly or through other stored paths.
- Consistency reports include every needed output of a completed build with no stored copy. They also include outputs that upstream caches hold.
- Builds no longer show as completed after a completion report from the worker if there's no stored copy of a required output on the server. The server does retry each build (three attempts by default). Those builds then fail with the output paths in the failure reason. Evaluation and commit status show failed instead of completed. There's nothing to fetch. Admins can check worker upload errors.

### Build Logs Through a Cache

- Build logs through a cache (`nix log`, `/cache/{cache}/log/{drv}` or SSH) come from the build attempt that produced the cached output. They don't come from the latest attempt of that derivation.
- Only trusted builds get logs.
- A cache can provide build logs in one case only. Projects that can read the cache need a build job for that derivation. The evaluation of the project was still running when the build attempt that produced the cached output started, or that evaluation started later.
- Projects can read a cache through a read-only or read-write subscription to an active cache.
- That build job doesn't count when build attempts for the derivation start after the last evaluation of that derivation in a project that can read the cache.
- Caches whose subscriptions are all write-only now return no build logs at all, because no project can read from them.
- Outputs built only for projects that don't read the cache have no log through that cache.
- Subscribe the project to the cache in read-only mode to restore these logs, or read the logs in the project.
- Outputs without a build job (user uploads, outputs stored before the upgrade) have no log through a cache.
- Such requests fall through to an upstream cache. With no hit upstream, the cache returns `404`.
- These logs still show on the build log page in the web UI.
- No schema or config changes.

### Cache Contents

- Caches no longer serve responses for NAR, file listing, single file, build log and debug info for paths without a cache signature. These requests return `404` like the narinfo response.
- Caches return every path with a signature from that cache. These paths stay available after worker deletion, after a subscription with `sign_outputs` off, and after the end of a project subscription. Signature records without a signature and inactive caches return nothing. Only new signatures depend on worker trust. The server can write new signatures only for trusted workers. Admins can delete one of these paths by deleting its NAR.
- Upgrades delete stored path records that lack a file hash, NAR hash or NAR size. They also delete their signature records and producer records. Only older releases wrote such records. Those records had no content. Columns `file_hash`, `nar_hash`, `nar_size` in `cached_path` still allow NULL. Rows without these values violate the validated CHECK constraint. These columns become NOT NULL in the next release. The upgrade needs no action.
- Upgrades delete stored path records that the server didn't confirm. They also delete the signature, producer and debug info records for those paths. Only instances that ran release v1.4.0 with S3 storage have such records. These records are worker uploads that were still staged when the instance upgraded to a version after v1.4.0.
- Count these records with `SELECT count(*) FROM cached_path WHERE NOT confirmed;` before the upgrade.
- Requests for these paths get `404` from the cache until a rebuild or another upload of that path.
- The upgrade set `walked` to false for every derivation whose `.drv` file or input source was one of these deleted records. Later evaluations traverse these derivations again and upload the missing paths.
- Periodic consistency checks include the demotion pass. The stored output of a completed build can be missing. The demotion pass can set that build back to queued within about 5 minutes. A running evaluation or a required parent build can request the change. One pass can process at most 64 builds. The same build can go back to the build queue at most once per 24 hours. Other builds wait until later evaluations request them.
- `GET /api/v1/board/health` removed the `unconfirmed_nars` field. No field can replace the removed one. Upgrades delete unconfirmed uploads, so the count can only be 0.
- All NARs on S3 storage larger than `nar.smallBytes` use presigned URLs for delivery.
- Stored narinfo fields with control characters or whitespace inside one reference produce a server error in place of rendered narinfo.

### Cache Subscriptions

- Read-only subscriptions can't sign outputs. The REST API (`400`) and state file reject `sign_outputs = true` for those subscriptions with this text. Those requests stored `false` before the upgrade.
- `sign_outputs` is optional. Read-write and write-only subscriptions sign by default. Read-only subscriptions don't sign.
- The type for NixOS option `services.gradient.state.caches.<cache>.projects.*.sign_outputs` is `null or bool` with default `null` (previously `bool` with default `true`).
- Read-only subscriptions with `sign_outputs = true` in the NixOS module fail an assertion at evaluation time.
- Existing read-only subscriptions stop signing outputs during the upgrade.
- Requests that enable signing return immediately. A background job can fill in signatures for earlier outputs. Time can vary with the number of outputs in the project.
- Servers backfill missing signed entries once per signing subscription in the background after upgrade. The job can cost ~1.3s per 100k output hashes of a project. The sign sweep can then sign these entries the same way as for a new subscription.
- Approved subscription requests and subscriptions from the state file now get signatures for earlier outputs. Only subscribe requests and change requests through the REST API added signatures before the upgrade.
- Stored signatures remain with ``sign_outputs`` off. New outputs stay unsigned.
- Project members lose the ability to view and download from a private cache when the project subscription to that cache is write-only. Project subscriptions to inactive private caches follow the same rule. Members with their own role in the cache keep that role's permissions.
- New worker outputs stay unsigned in the cache with signing off for that subscription. New uploads over SSH and build requests over REST also create no signature or upload record in that cache. Signatures from before the upgrade remain.
- Projects with signing off for a cache subscription store paths uploaded over SSH (`nix copy --to ssh-ng://...`), but the cache can't read them back. The project's SSH store and readers outside those caches can't fetch them again.

```text
A ReadOnly subscription can't sign outputs. Change the mode to ReadWrite first.
```

### Download Links

- Download links point only to build jobs of the evaluation that created those links.
- Shared links to a build job of another evaluation respond with `404`. Users need a new link.

### Signatures on Pull

- Pulls by workers and pulls from a cache include only signatures from a cache that the reader can read.
- Clients that trust only the keys of another cache get no signature that they can verify for that path. Add the keys of a cache that the client can read.

### Uploads

- Workers can upload a path that the server already stores again. The projects of these workers get their own record of that content.
- Missing uploaded objects cause the commit to fail with the text below. Expect the worker to upload the content again automatically.
- REST uploads need free storage for one extra copy of the NAR at the end of the upload.
- Presigned and multipart uploads to S3 create one extra copy in S3. Uploads over 5 GiB pass through the server one more time.
- The server rejects REST uploads, worker uploads, and uploads by `nix copy` over SSH in these cases.
- A store path that isn't a valid Nix store path name. A valid name has a hash of 32 nix32 characters and a name of up to 211 bytes from `A-Za-z0-9+._?=-`. The name is not `.` or `..`. Stored rows with other names still load.
- A full store path field that names a subpath such as `/nix/store/<name>/bin`.
- A specified reference that isn't a store path or doesn't exist in the NAR. Empty references in worker metadata now count as rejected.
- A deriver that isn't a `.drv` store path. An empty deriver or `unknown-deriver` still means no deriver. An empty `ca` still means no content address.
- A content address that's malformed, doesn't match the content, or doesn't produce a store path.
- A `.drv` upload where the store path doesn't follow from its text and references. Old clients that upload fabricated `.drv` content fail.
- A NAR file from a REST upload without zstd compression, or a NAR file whose decoded bytes differ from `NarHash` or `NarSize`.
- Rejected REST uploads get `400` with code `input_validation`. Rejected worker uploads fail the job with no retry. Uploads by `nix copy` over SSH fail and show the cause.
- Chunked REST uploads that started before the upgrade don't resume. Clients redo them from offset 0. The `gradient` CLI remains unaffected.

```text
The commit failed. The uploaded object didn't exist in storage when the commit started. A deletion after the upload can explain the missing object. Upload the content again. (`<path>`)
```

### Worker Access

- Connected workers lose revoked projects or capabilities without a restart.
- API changes take effect immediately. Direct changes to the state file, SCIM, or database take effect within 60 seconds.
- Running jobs of a worker that lost a project or required capability stop and return to the queue for other workers. A reconnect without that access can end the same way.
- Planned cluster seats for that worker become free and never start.
- Team workers that go inactive or lose their last project grant disconnect.
- Worker token rotations remove that project from open sessions. Sessions close when no projects stay.
- Team workers without a project grant fetch no jobs for any project. Before, they fetched jobs for all projects.
- Re-enabled capabilities come back without a reconnect, but only if the worker reported them at connect time.
- Worker sessions close after three database read failures in a row within about 3 minutes.
- SSH keys, SSH host keys and logins for HTTPS Git of a project travel inside `AssignJob`. No separate credential messages remain.
- Workers and the server communicate over wire protocol 67. The server cannot accept workers at version 2.0.1 or earlier. The server cannot accept workers built from main before this change. Upgrade the server and workers together.
- Jobs and input fetches of a fork pull request without approval never receive the logins for HTTPS Git of the project, also with `require_approval: false`. Such fetches run without credentials.
- Fetches of this kind that fail produce a message that the server withheld credentials. These messages carry the comment `/gradient run <full 40-character commit hash>`.
- SSH inputs on such a pull request fail immediately with the same message.
- No schema or configuration changes.

### Worker Imports

- Workers reject a store path from the server cache or an upstream cache when the response has no NAR hash.
- Workers report one of the errors below for that path.
- An upstream cache that serves narinfo files without `NarHash` can no longer act as a substitute source. Normal Nix binary caches always set `NarHash`.

```text
cache response for <path> has no NAR hash, so there's no way to check the NAR before import
The upstream response for <path> contained no NAR hash. The worker can't check the NAR before it imports that archive as a build output.
```

### Remote Builds over SSH

- Build requests over SSH (`nix build --store ssh-ng://...` or `--builders`) fail when derivations in them state output paths that don't match inputs. Checks apply to the requested derivation and every derivation in its closure.
- Derivations from Nix itself are not affected.
- Floating content-addressed derivations remain refused as before.
- Refused requests fail with one of the messages below.

```text
<drv> shows output <output> at <stated path>, but its inputs give <computed path>.
Can't check the output path of <drv>. The input has no known output path.
```

- Open connections to the SSH store kept their access until closed in earlier releases. Rechecks now happen every 60 seconds and right after related audit events. Rechecks determine whether the SSH key can still log in and which caches a client with that key can read. Gradient must close the connection after a changed result or a failed recheck, such as a database error. Clients can reconnect.

### Two Server Instances

- A server instance takes a lock in the database for schema preparation. Only one instance can run a migration at a time.
- Second server instances wait on that lock. The waiting second instance starts on its own without a restart after the first instance has finished the migration.

### Refresh of Waiting Evaluations

- Two dedicated database connections handle the refresh of waiting evaluations. Statements on these connections stop after 5 seconds and lock waits stop after 2 seconds.
- Refresh failures for one evaluation remove that evaluation from the rest of the current pass. Log entries contain one warning for each streak of failures. Warnings include the error kind and SQLSTATE. Set the log level to debug to get the complete error text.
- Refresh passes stop after 30 seconds. Dispatch is next. Skipped evaluations come first on the next pass. Log lines report the spent time budget at info level.

### Database Migration

- Release 2.1 doesn't support server instances from the previous release that run alongside 2.1 instances.
- Stop all server instances of the previous release before the first 2.1 instance can start.
- Instances of release 2.0.1 fail nearly all requests against a database that release 2.1 migrated. Queries for users, tasks, teams and integrations fail.
- Servers on release 2.0.1 can delete the migration records for release 2.1 at startup on a database that release 2.1 migrated. Later starts of release 2.1 fail at the first migration.
- Never start an older server against a migrated database.
- Back up the database before the upgrade.
- Restore that backup, the only rollback.
- NAR files that release 2.1 uploaded have no database rows after the restore. Storage sweep jobs in release 2.0.1 delete those files after the upload grace period.
- The steps in migration `m20261221_000001` take an ACCESS EXCLUSIVE lock on `cached_path` inside one transaction. Multiple full scans of the table occur in that transaction. The lock is in place until the end of the migration on the first server start after the upgrade. Large instances see a delayed server start. Requests get no response from the cache during that time.
- Migration `m20261221_000001` must rewrite the stored path table (`cached_path`) and its signatures once. The server can start only after that rewrite. A test machine with 128 cores and the database in RAM took about 2.3 minutes per million stored paths. Expect a longer time with a disk-based database. Plan for free disk space about the size of the stored path table. Table size can double until the next vacuum. Measurements show 782 MB to 1563 MB per million stored paths.
- Several migrations read or rewrite all rows of a table. Migrations `m20261221_000001`, `m20261222_000000`, `m20261223_000000`, `m20261226_000000`, `m20261227_000000`, `m20261230_000001`, `m20261231_000000`, `m20270101_000000` run in one transaction each. Migrations `m20261221_000002`, `m20261230_000003`, `m20270102_000000` run each statement separately, outside a transaction. The statements follow.
    - `m20261221_000001`: UPDATE of `cached_path.nar_object_storage_key`, INSERT of one legacy row per `cached_path` row into `store_path_content_producer`, UPDATE of `cached_path_signature.store_path_hash`.
    - `m20261221_000002`: DELETE of duplicate `cached_path_signature` rows.
    - `m20261222_000000`: UPDATE of `user`.
    - `m20261223_000000`: UPDATE of `team_user`.
    - `m20261226_000000` and `m20261227_000000`: UPDATE of `derivation_output`.
    - `m20261230_000001`: UPDATE of `derivation` with the build limits of all `derivation_build` rows.
    - `m20261230_000003`: DELETE of `eval_cache_store` rows without a project and of duplicate `eval_cache_store` rows.
    - `m20261231_000000`: UPDATE of `role` and `api`.
    - `m20270101_000000`: DELETE of all `cli_device_authorization` rows.
    - `m20270102_000000`: run the DELETE of `cached_path` rows with no file hash, NAR hash or NAR size, and the UPDATE of `derivation` rows whose `.drv` file or input source was removed, in one transaction. Run a VALIDATE scan on `cached_path` after the transaction.
    - `m20270103_000000`: run DELETE of `cached_path` rows with unconfirmed upload and UPDATE of `derivation` rows whose `.drv` file or input source was removed, in one transaction.
- Plan the upgrade as a maintenance window if any of `cached_path`, `cached_path_signature`, `derivation`, `derivation_build` or `derivation_output` contain more than 20 million rows.
- Migration `m20270105_000000` used `CREATE INDEX CONCURRENTLY` to create index `idx-store_path_content_producer-cached_path-kind` on `store_path_content_producer` (columns `cached_path` and `kind`). The index build doesn't block writes. More rows in that table make the index build take longer.
- The 2.1 database migrations wait up to 5 seconds for a table lock. A migration that can't acquire the lock within 5 seconds causes the server startup to fail and the server to stop. Restart the server.
- Release 2.1 does not write to column `cli_device_authorization.token`. Expired sign-ins clear it. The next release does not keep that column.
- Build limits come from `derivation` in release 2.1. Columns `derivation_build.timeout_secs` and `derivation_build.max_silent_secs` fall out of use in that release. Both columns leave the schema in the next release.
- Column `cached_path.confirmed` and index `idx-cached_path-unconfirmed` have no use in release 2.1. Both disappear in the next release.
- Column `eval_cache_store.project` is NOT NULL in the next release.

### Repository URLs

Tasks and build requests accept only these formats as the repository URL. All formats except scp work with the `git+` prefix.

- URLs for `https://`, `http://`, `ssh://`, and `git://`.
- scp format with a user, such as `git@host:path`.

API requests with other URLs receive a refusal response with the text below.

- Local paths, `file` URLs, and flake references such as `path:` and `github:`.
- The `host:path` form with no user. Use `git@host:path` or `ssh://host/path` instead.
- URLs with `#` or a backslash.
- URLs that have ``-`` as the first character of the host or user.
- `git://` URLs with something other than a port number after `host:`. Port 9418 was the fallback before.

```text
Invalid repository URL; use ssh://git@host/path, git@host:path, https://host/path or git://host/path
```

- Saved tasks that contain a refused repository URL fail with an invalid URL error on every poll. The only way to resume polling is an edit of the repository URL by an admin.
- Workers permanently fail jobs that have a refused repository URL.
- Admins check the repository URL for each task in the task settings or in `tasks.<name>.repository` in the state file. Rewrite rejected URLs to an allowed format. Rewrite `github:owner/repo` to `https://github.com/owner/repo`, for example.
- Build requests that contain `rev` follow the same rules.
- Build requests that use a `git://` URL save the repository as `git+git://...`. Tasks work the same way.
- Input updates fail for inputs of type `git` in `flake.lock` with a local path.

### Code Source for Fork Pull Requests

- Gradient can reject a clone URL of a fork. Evaluations of fork pull requests fetch from the task repository in that case.
- Clone URLs of a fork also give the same result when the host or the explicit port differ from the task repository. A URL with a port and a URL without a port count as different.
- These evaluations still build the head commit of the pull request.

### Base URL for a Connection to a Git Host

- Gitea and Forgejo connections require `base_url` in both the API and the state file. Requests without that field return `422` together with the text below. GitLab connections without `base_url` use `https://gitlab.com`.
- Base URLs with a user, password, query, or fragment fail validation.
- GitLab connections without a base URL receive `https://gitlab.com` during the upgrade.
- Gitea and Forgejo connections that were saved without a base URL remain. These connections stop working. Changes to these connections need a new base URL.
- Admins set `base_url` for these connections after the upgrade. State files with a Gitea or Forgejo connection without `base_url` or a task whose repository host and connection host differ fail the NixOS system build. NixOS system builds include a state check with `--state-validate`.

```text
Connections to Gitea or Forgejo need `base_url`, the http or https address of the Git host.
```

### Git Host Connections

A task can use an instance connection or a team connection only with a grant for an exact repository match in the project. A project connection can operate without a grant.

- Tasks that already use a connection receive repository access from the upgrade. Upgrades grant this access to each instance connection or team connection. These grants also cover an active action to open pull requests through such a connection. Their source is "upgrade".
- Tasks from the state file receive a grant with source "state file" on their first start after the upgrade. The "upgrade" grant for the same repository does change its source to "state file".
- A task with a repository URL without an owner and name can lose the connection and status reports.
- No grants go to tasks in a project outside the team of the connection. The connection is still configured, but Gradient does reject the connection. There are no status reports, pull requests or webhook triggers.
- Gitea and Forgejo connections don't work for tasks without a base URL (instance, team, project) anymore. Set the base URL first. Webhooks don't start those tasks. Gradient can't send the connection token to the host in the task URL.
- After the upgrade, a task must have a grant in that project to use an instance connection or team connection. A grant is necessary for connection attach. A grant is also necessary for a new action to open pull requests through the connection. A grant is also necessary for a task move to another project.

**Grant Management**

- Superuser can manage grants for instance connections in `/api/v1/admin/git-host-connections/{id}/repository-grants`.
- Team Admin can manage grants for team connections in `/api/v1/teams/{team}/git-host-connections/{id}/repository-grants`. These grants apply to projects in that team only.
- Use `GET` to list grants, `PUT` to create grants and `DELETE .../repository-grants/{grant}` to delete grants.

**State File**

State file tasks in any of these cases halt server start, with the task name in the error.

| Case | Fix |
|---|---|
| Team connection for a project outside that team | Move the project into the team, or use an instance or project connection |
| Gitea or Forgejo connection without `base_url` | Set `base_url` on the connection |
| Repository URL on another host than the connection | Correct the repository URL |
| Repository URL without owner and name | Correct the repository URL |

- Grants that use `state file` as source follow the state file. They disappear after removal of the task from the state file.
- Admin-created grants remain.
- `upgrade` grants without a task in the state file remain until deletion by an admin.

**API Clients**

- Requests with a connection that the task can't use receive `403` with the text below, instead of the former `400` texts.
- New audit events: `git_host_connection.repository_grant.create` and `git_host_connection.repository_grant.delete`.
- Repository names with GitLab subgroups, such as `sub/app`, work in grants. Names with the `.git` suffix and owners or names above 255 characters receive `400`.
- A `PUT` for a repository with an "upgrade" or "state file" grant creates an admin grant with `200`, not a `409`.
- Entries of a team grant list can have a null `project_name` for a project outside the team.

```text
This task can't use Git host connection {id}: This connection has no grant for this repository in the task's project.
```

**Connect Flow**

- Users now complete the Gitea and Forgejo connection flow for a project on the project page of the signed-in user.
- Connection flows in progress during an upgrade fail once with a message. Retry the connection.
- The redirect URI registered at the Git host is still `{serve_url}/api/v1/git-host-connections/oauth/callback`. Gitea and Forgejo need no change.

### Editing a Connection to a Git Host

- Edits that change `git_host_type` delete the stored access token, its refresh token and expiry time, and the webhook secret of the connection. Edits that change the scheme, host, or port of `base_url` delete the same values.
- Project, team, and instance connections follow the same rule.
- A trailing slash, an explicit default port, and a change to the path alone do not count as changes. A switch from https to http on the same host is a change.
- The `access_token` or `secret` values from the same request go to storage in Gradient.
- Connections without them get no access token afterwards. The connection looks the same as a connection that never had an access token in that case.
- Paste the token again, or sign in again for a Gitea or Forgejo connection created through sign-in. Install the webhook again, or set a new secret.
- Connections that receive events get a new webhook secret with that edit if the request doesn't send `secret`. Responses show the new secret once in `generated_secret`, same as on creation. Webhooks on the Git host need the new secret.
- Connections that report commit statuses reject such an edit with `400` and the error text below for requests with no new access token. Such requests save nothing. The fields of the connection keep their current values.
- Tokens from sign-in via the administrator's OAuth2 application work only on connections whose base URL has the same scheme, host and port as the application. Gradient rejects all other uses of these tokens.

```text
The Git host URL and access token are required to report commit status.
```

### Token for the Matrix Action

- Matrix action edits fail with `400` and text below on homeserver URL scheme/host/port change without new `access_token`. Tokens remain. Path-only changes preserve them.

```text
Send the access_token again if the homeserver URL moves to another scheme, host, or port.
```

### Credentials for an Upstream Cache

- Upstream caches that use HTTP accept `credential` in the API, either `basic` with `user` and `password`, or `bearer` with `token`.
- The state file can take `basic_auth_user` with `basic_auth_password_file`, or only `bearer_token_file`.
- Credentials need a `https://` URL. Credentials for a `http://` URL fail validation.
- Usernames that contain `:` don't pass validation. Empty or whitespace values for `basic_auth_user` make state file validation fail.
- Upstream URLs with a user, password, or query get `400`. Clients of the API put the user and password in `credential`.
- The API response includes only `credential_scheme`, not the credentials themselves.
- `PATCH` requests that move the URL to another scheme, host or port must send `credential` again, or `{"scheme":"none"}`.
- Workers never fetch from an upstream cache with a stored credential. Only the server can fetch from such caches, for pull-through, build logs and the Test route.
- The column `cache_upstream.api_key` now has the name `encrypted_credential`. Hand-written SQL needs the new name.

### Existing Upstream Caches

The server checks every saved upstream URL on the first start after an upgrade.

- User and password in the URL of an HTTP upstream cache move into a stored `basic` credential.
- The moved credentials still work on `http://` URLs. There is a warning in the log. Change these URLs to `https://`.
- Upstream caches with a query in the URL become inactive.
- HTTP upstream caches that hold a stored credential and a user or password in the URL become inactive.
- Upstream caches for Gradient Proto go inactive when their URLs contain a username and password.
- Inactive HTTP upstream caches need a fixed URL through `PATCH`. `active=true` is necessary after that. Upstream caches for Gradient Proto need a new entry.
- `PATCH` requests that carry `active=true` recheck the stored URL.

```text
Upstream cache is now inactive. The stored URL has a query, or the user and password can't move to credentials. Fix the URL, then set the upstream active again.
```

- Stored NAR URLs that contain a user, password, or query receive a one-time clear. Pre-signed links with a query receive the same one-time clear. Later lookups store these URLs again.
- Deleted upstream caches lose their stored NAR URLs.
- Upstream metrics use the old URL as key. Charts show two entries for each upstream cache until the end of the retention period.

### Upstream Cache Limit

- A cache can have up to 32 upstream caches.
- Caches above the limit keep working. New upstream caches receive `403` until fewer than 32 remain.
- State files with more than 32 upstream caches for one cache fail validation. The server can't start until the list is shorter.

### Hidden URL Parts

- The upstream list is one of five outputs that omit the user, password and query parts of stored upstream URLs. The other four are state export, evaluation waiting reason, report export and cache board.
- Upstream lists now have a new field `stored_url_has_user_password_or_query`. The value is `true` for a stored URL with a user, password or query part.

### Upstream Narinfos

Upstream cache narinfos now go through a single strict parser on the server. Narinfos that Nix, cache.nixos.org and common cache servers write remain unaffected.

- Rejected forms include a key repeated twice (except `Sig`), a `Key:value` with no space, CRLF line endings, whitespace around a value and empty lines (including the second trailing newline).
- Rejected forms also include keys with characters outside ASCII letters and digits, plus a `References` entry or `Deriver` that's not a store path name.
- `FileHash`, `FileSize` and `Compression` stay optional.
- The server treats a rejected narinfo as missing from that upstream cache. The server does try the next upstream cache. The request receives `404` when no cache is left. Server logs contain the reason.
- The server renders the narinfo of an upstream path from checked values in a fixed key order. Unknown keys (such as `System`) no longer pass through.
- The JSON format does not include `FileHash` and `FileSize` when both fields are absent from the upstream cache.
- No schema or configuration changes.

### Destinations in Private Networks

The server connects only to public IP addresses for upstream cache URLs, Git host API URLs, action URLs, outbound webhook URLs, Git repository URLs of tasks (git, http, https, ssh) and the SSH host key scan by default.

- Gradient does not accept hosts with loopback, private or link-local addresses. Gradient also does not accept hosts with an address of one of the server's own network interfaces.
- Admins list the allowed targets with the NixOS option `outbound.allowedNonPublicNetworkTargets` (`GRADIENT_OUTBOUND_ALLOWED_NON_PUBLIC_NETWORK_TARGETS`). Private targets have no entry in the default value (`[ ]`).
- Entries accept a CIDR range, an IP address, or an exact-match hostname. Add the API address and the repository address of self-hosted Git hosts and internal caches here.
- Existing upstream caches and webhooks with a private address or the server's own address stop working after the upgrade until they appear in this option.
- Tasks that have repositories on private addresses fail to fetch after the upgrade. Fetch can resume only after that host is in this option. The standalone image can read this option from the `GRADIENT_OUTBOUND_ALLOWED_NON_PUBLIC_NETWORK_TARGETS=<targets>` line in `/var/lib/gradient-standalone/server-environment`. HTTPS Git connections share certificate sources with other Gradient connections (system store, `SSL_CERT_FILE`, `SSL_CERT_DIR`) and ignore `NIX_SSL_CERT_FILE`, while server certificates need a subjectAltName host name.
- Workers apply the allowlist of the Gradient server that they connect to when their jobs open Git connections. Workers receive the allowlist at session start and have no configuration of their own. Git connections from the Gradient server and workers fall under the allowlist, not downloads Nix makes itself, such as flake inputs and fixed-output builds.
- Refused upstream caches produce one warning each in the log. Changes in the refusal reason produce the warning again. The log fields are `upstream`, `host` and `reason`. The warning prefix is `The server refused to fetch from this upstream cache.`.
- Rejected webhook deliveries show the text below in `error_message` of `GET .../webhooks/{id}/deliveries`.
- Rejected repository checks and host key scans display the same text as rejected Webhook deliveries. That text is below.
- Hosts in `giteaApp.instanceBaseUrl` stay reachable without an entry.
- The upstream host must resolve at creation time. An HTTP upstream host must also resolve at `PATCH` time for the URL.
- Hostnames without an address, or with only refused addresses, receive the text below.
- A failed read of the addresses of the network interfaces can block every destination. Manual server units and worker units need `AF_NETLINK` in `RestrictAddressFamilies`.

```text
The server cannot connect to <host>. The name does not resolve, or the server administrator does not allow that address.
```

### CLI Downloads

- `download`, `builds sbom`, `evaluation report` and `exec-job artifact download` leave existing files in place. These commands fail with the text below.
- Pass `--replace-existing` to these commands to replace files.
- Files from `gradient build` always overwrite existing files in the result directory.
- The CLI does still write directly to paths that `--output-file` or `--output-file-path` name. `/dev/stdout`, FIFOs and symbolic links work as before.

```text
<path> already exists. Pass `--replace-existing` to replace it.
```

The `gradient build` and `gradient download` commands do not write to a `result` directory or a subdirectory of it. These restrictions take effect when that path is a symbolic link or a regular file. Links from `nix build` to `result` also count. Remove the link or choose another directory name.

```text
Refused to write into <path>: it is a symlink or not a directory. Remove it and run the command again.
```

File names from the server need to be plain file names. The names below stop the download.

- A name with a directory part, a leading dot, or `:`.
- Names that contain control characters or characters that change text direction.
- Names that end with a dot or a space.
- Reserved device names for Windows (such as `CON` and `aux.txt`).

`builds sbom` and `evaluation report` fall back to a fixed file name instead.

### Terminal Output of the CLI

- Logs, names and error texts from the server reach the terminal with colours only. Other escape sequences, control characters and text direction characters do not reach the terminal.
- Progress lines that a carriage return overwrote show only the final state. The output from `10%\r20%` is `20%`.
- Log lines display at their newline. Final lines without a newline display at the end of the stream.
- Literal escape sequences in logs, such as `\u001b`, `\n`, `\t`, stay as plain text.

### MCP Server

- `run_gradient_command` can run only the commands that appear on its list. Commands that have no entry on the list can't run.
- Newly refused commands are commands that set secrets or write local files, `git-host-connection create` and `edit`, `cache upstream add` and `exec-job console`.
- The server rejects `project select` and `task select`. MCP clients pass `--project` and `--task` instead.
- Users who relied on a command that MCP refused run it in a terminal themselves.
- Refusals answer with the text below.

```text
`gradient <command>` can't run here. <reason>
```

### Input Fetching and Build Reports

- Evaluations of different projects no longer share an input fetch. Instances with many projects that use the same inputs see more fetches.
- The new option `scheduler.inputFetchTimeoutSecs` can set the time that workers get to respond to input fetches. The default is `3600` seconds.
- A different worker that can fetch that input takes over a fetch with no response in time. Waiting evaluations fail with the first text below if no such worker can take over.
- Input fetches that no worker can take fail their waiting evaluations with the second text below. Count the same time from the first request.
- The server stopped asking the same worker for the pinned fetch after that worker returned a different store path. The fetch can go to another worker. Otherwise the evaluations fail with the third text below, where both store paths appear.
- The server accepts only one report of the outputs of a build per dispatch. The only source of that report is the worker that the dispatch assigned.
- Build output reports that arrive after a closed dispatch get no processing at the server. Closed dispatch states include requeue, build end, and abandonment.
- Workers on the same release need no changes. Discarded reports show up in the server log as warnings that contain the final text below.
- The server filter can exclude build artifacts from a build report for empty path segments, `.`/`..` segments, backslashes, and control characters. The server can write a warning to the log. The build can stay unchanged.

```text
No worker responded to the input fetch within <seconds> seconds
No worker accepted the fetch of input <input> within <seconds> seconds
The worker answered the input fetch with store path <answered>, not with the store path <pin> that the content hash of the request names.
Ignoring a second report for a dispatch, or build output when this worker has no active dispatch.
```

### Evaluation Cache

- Segments of the evaluation cache now belong to one project, even when several projects use one shared cache. Projects no longer reuse segments from each other.
- The upgrade deleted all existing segments once. The database migration removed the rows. A one-time cleanup during storage maintenance removed the stored objects.
- The first evaluation of each flake and project starts cold after an upgrade. It can take longer.
- All evaluation cache fetches get a miss until the clean-up finishes. Compaction can't run before then. The server logs contain the text below.
- Admins can see clean-up completion from row `m20261230_000000_delete_eval_cache_segments_modified_before_project_column` in table `storage_migration` with `applied_at` set. Log lines stop after that.
- Cleanup operations also remove segments uploaded within 15 minutes after the database migration. Affected projects run one more cold evaluation.
- Only evaluation jobs can pull and upload segments. The server does not accept uploads from build jobs or exec jobs. Custom worker builds must not upload segments from those jobs.
- The upgrade discarded compactions that were queued before it. A warning appeared in the log.
- Project deletions remove the segment rows of those projects. Sweep runs remove stored segments without a row once those segments are older than `gc.narUploadGraceHours`.

```text
All evaluation cache pulls get a miss until the storage migration has run.
The code doesn't compact the eval cache until the one-time deletion can apply to segments written before the upgrade.
```

### Password Sign-In

- Five attempts with a wrong password on one account start a wait. Sign-in with a password and the password check for account deletion then return HTTP status `429`, error code `rate_limit_exceeded`, and a `Retry-After` header in seconds. Wait periods start at 1 second, double after each further wrong attempt, and reach a maximum of 15 minutes.
- Attempts that fail with an error or that the client cancels count as wrong password attempts.
- Requests skip the wait if the account has a usable browser session on the network. Sessions from `gradient login` don't count. A network is an IPv4 address or an IPv6 /64.
- Other networks receive `429` even with the correct password while the wait is active. A third party who knows the account email address can keep sign-in with that account password in a wait state. The user can still sign in from a network that has a valid session for that account. The user can also sign in through an external identity provider, an existing session, or an API key.
- Wrong-password counters live in process memory. A server restart clears them. Multiple server instances don't share them.
- Parallel slots serve password hashing. Their number is at most half of the CPU cores on the server. Sign-in, registration, account deletion and creation of worker tokens and connection tokens return `503` with `Retry-After: 5` when no slot is free within 5 seconds. These `503` answers don't count as wrong password attempts.
- API clients wait for the time in `Retry-After` before a retry.

### IPv6 Rate Limits

- Rate limits group IPv6 clients by /64 network, not by individual address. Store routes over gRPC follow the same rule. Clients in one /64, such as an office or a pool of CI runners, share a single limit.
- Reverse proxies outside `http.trustedProxies` put all clients into one network. These clients share one limit. A usable browser session can exempt all clients from the wait after wrong password entries. Add the proxy range to `http.trustedProxies`.

### Session Lifetime

- Sessions from `gradient login` end at the latest 30 days after sign-in. All other sessions end at the latest 90 days after sign-in.
- Renewals no longer extend a session past that end.
- Sessions older than that end at the upgrade. Their users sign in again.
- The session list shows only sessions that are still usable.

### CLI Sign-In

- Sign-ins that started before the upgrade and did not complete with `gradient login` no longer survive the upgrade. Run `gradient login` again.
- The approval page stopped reading the code from the link. Users type the code shown in the CLI.
- Server links no longer contain a code. `verification_uri_complete` and `verification_uri` now have the same value. Scripts against the API see `verification_uri` and `user_code` instead.
- CLI sessions start at approval collection, not at approval in the browser.
- CLI calls receive `cli_auth_expired` after approved browser sessions end or users become deactivated in the meantime.

### CLI Session Limits

A session from `gradient login` can't perform the actions below. These requests receive a `403` with the code `cli_auth_needs_browser_session` and the text below.

- Create, change, revoke, or delete API keys.
- Add or remove SSH keys on the account.
- Change the username, name, or email address.
- Delete the account.
- Link or unlink sign-in providers.
- Approve or deny a sign-in from the CLI.
- End other sessions.

```text
A sign-in from the CLI can't do this. Sign in through the browser.
```

Users sign in through a browser for these actions. Scripts and CLIs can use API keys created in the browser. API keys receive `403` on these routes as before.

### Derivation Files

The server records each derivation only from a validated `.drv` file. Evaluation workers upload each `.drv` file before reporting a derivation.

- Upgrade the evaluation workers together with the server. Workers in this release report a derivation after its input derivations.
- Old evaluation workers fail evaluations where new derivations span multiple report batches. These evaluations display the first text below.
- Older evaluation workers sometimes report a derivation before the `.drv` upload is complete. These evaluations fail with the second text below.
- The first evaluation of each project is slower after the upgrade. That slowdown is one-time. Workers report all derivations again and upload the `.drv` files to verify them again. Later evaluations report only new derivations.
- Evaluations fail with the second text below if the reported `.drv` file is absent or untrusted, or its store path doesn't match. Trusted files come from workers of the project. Trusted files also have, as in [Trusted Build Outputs](#trusted-build-outputs), the signature of a cache that the project can read.
- Evaluation also fails for `.drv` files when the output paths don't follow from their content.
- Worker reports of system, outputs, inputs, and build limits have no effect on the server. Those facts come from the `.drv` file.
- The build limits `timeout_secs` and `max_silent_secs` come only from the `.drv` file. Later evaluations of another project no longer change the limits of a shared build. The API fields and the report table `derivation_build` keep both names.
- Build requests over the `ssh-ng://` store require the `.drv` files of the request in the project caches. The project must trust those files. Refusal messages show the file to copy, as in the third text block below.
- Evaluations fail with the fourth text below when the server cannot save a batch of reported derivations. Such evaluations completed without those derivations before. The cause is only in the server log. Re-run the evaluation.
- Nix clients receive the same text for build requests over SSH when the server can't save the derivations.

```text
The server rejected the derivation file that was reported: <input> is an input derivation of <dependent>. The evaluation worker has not reported the `.drv` file for <input>. The database does not hold a verified hash modulo for <input>. Two causes are possible.

Evaluation workers older than the server report dependent derivations first. Upgrade the evaluation worker together with the server.

The cleanup of old unreferenced derivation rows deleted the row for <input> during this evaluation. Run the evaluation again.
```

```text
derivation file <path> was not uploaded or does not match its path
```

```text
<path> is not in the project caches, copy it first
```

```text
The server failed to save the batch of derivations. Evaluation can't continue: <cause>. Run the evaluation again. The server log records the details.
```

### Cache Uploads

The checks in [Uploads](#uploads) apply to uploads from all workers.

- The server records the derivation of a build job as the deriver of the build output. A worker declaration of a deriver can't change this.
- A build job can upload paths that are not enumerated outputs of the derivation. Those uploads have no deriver. Closure paths and floating content-addressed outputs are examples.
- All readers of a cache several projects write to get a path's first signed content from any project's worker. They trust all these workers.

### Build Estimates and Memory

- Builds with predicted RAM above the RAM of all workers the project can use no longer fail with reason `not enough resources`.
- Builds that fit no worker even at 1 core with RAM proportional to cores wait for the largest connected worker that the project can use. They run there. Smaller workers reject them.
- These builds no longer fail before an out-of-memory kill. Kills at 1 core end the build with the text below.
- Attempt records from before the upgrade keep reason `not enough resources`.
- Projects collect their own samples only from trusted workers. All samples from all projects still feed the shared estimate. Their own samples cover predicted RAM, build time and the lower bound for RAM. Kills on trusted workers count toward core limiting after an out-of-memory kill. The project's own kills on any worker also count. Register project workers or grant Workers [grant](../concepts/teams.md#grants) to a team on the project to get own estimates.
- Samples from before the upgrade have no worker information. They count only toward the shared estimate across all projects. Builds after the upgrade use that shared estimate. Only builds on workers trusted by the project end the use of the shared estimate once metrics arrive.
- The stored metrics keep the declared values when reported peak RAM or core count is higher than the worker declaration. The stored metrics keep the sent numbers when the reported core count is higher than the number sent with the job. Reports of 0 keep no value.
- Retries after an out-of-memory kill always finish. Kills qualify for a free retry on fewer cores when three conditions hold. Attempts require a known project. Builds use more than 1 core. Builds use fewer cores than every earlier kill of that derivation in the same project. All three conditions trigger the free retry. Other kills consume a build attempt.
- Kills recorded for another project on a worker that this project does not trust do not lower the core count for this project. Kills of other projects never block the free retry.
- The new nullable columns `worker_registration`, `team_worker`, `project` in table `derivation_metric` come from a migration. No action needed.

```text
ran out of memory with 1 core
```

### Exec Guest Directories

- Guest supervisors reject guest directories owned by root. Set directory ownership to the worker user, as in the NixOS module.
- Guest supervisors keep ``control.sock``, ``console.sock``, and ``console.log`` in ``/run/gradient-guests/<guest id>``. Create that directory, give root ownership, and clear write bits for group and others.
- The NixOS module creates that directory through ``RuntimeDirectory`` in ``gradient-guest@``. Custom units create it as root, with group ``gradient-worker``, mode ``0750``.
- The virtiofsd socket moved from ``<guest directory>/mounts/<tag>.sock`` to ``/run/gradient-guests/<guest id>/<tag>.sock``.
- Supervisors reject ``serial.sock`` and ``vsock.sock`` if the paths are symlinks or belong to another user.
- Supervisors open guest directories with ``openat2``. Worker hosts need Linux 5.6 or later.
- Stop running guests before the upgrade. New workers don't find the control sockets of old guests. Old supervisors misread the changed ``PrepareMount`` requests.

### Exec Mounts

- No `exec.mounts.*.path` can contain a symlink. All ancestor directories belong to root. They have no group or other write bits.
- Jobs that request a mount with a path outside these rules fail permanently without retry. Failure messages list the directory and the reason.
- Move such a path outside a directory such as `worker.baseDir` and set the resolved path.
- The rules reject a mount `path` that is exactly `/`.
- Supervisors create the per-job directory `<mount path>/<guest id>`. Only the worker user can own an existing directory at that path. Other owners, root included, cause permanent job failure.
- Supervisors reject a read-only mount that jobs request as writable.
- Mount names can contain 1 to 64 ASCII letters, digits, `-` or `_`. The names `work`, `control`, `console` belong to the worker.
- Mount names are unique per worker. Use different names to assign different paths to different projects.
- The worker and supervisor do not start with an incorrect mount name. NixOS evaluations fail with the text in the block below.

```text
duplicate mount name "<name>". Each mount must have a unique name on the worker.
services.gradient.worker.exec.mounts: invalid mount name for <names>. Mount names can contain 1 to 64 ASCII letters, digits, -, and _. The names work, control, and console belong to the worker.
services.gradient.worker.exec.mounts: the mount names <names> appear on more than one entry. Each mount needs its own name.
```

### Worker Resources per Project

- `services.gradient.worker.exec.sops.files`, `exec.mounts` and `exec.devices` entries need `projects`. The value is `"all"` or a list of project UUIDs. NixOS evaluations fail without it.
- Entries in `exec.sops.files` change from a path to `{ path = ...; projects = ...; }`.
- `GRADIENT_WORKER_EXEC_SOPS_FILES` is a JSON list of `{ "path": ..., "projects": ... }` entries. Workers reject the old comma-separated list at startup.
- Jobs for other projects fail permanently with the same error as a missing secret, mount or device class.
- Jobs that request a device class the worker doesn't offer to that project fail permanently. Before, a job waited for a free device.
- Exec jobs go only on workers that offer the secrets, mounts and device classes for the project of the job. The scheduler is responsible for that placement.
- Server secrets with the same name as a worker secret for another project no longer fail with "found in two sources".
- Secret names must be unique across all sops files for one worker and across files for different projects. Duplicates stop worker startup with the text below.

```text
secret "<name>" found in two sources: <first>, <second>
```

### Exec Work Disk

- Exec jobs without ``diskMb`` receive a work disk of 2048 MB by default, not a shared host directory.
- The checkout must fit. Larger checkouts fail the job permanently with the text below.
- Such jobs reserve 2048 MB in the worker's disk budget. Smaller disks run fewer jobs at a time.
- Guest images with an agent below guest protocol 5 fail with a permanent error. Rebuild custom guest images.

```text
the checkout does not fit in diskMb = <n>
```

### Local Worker on the Server Host

- The local worker on the server host (`services.gradient.localWorker`) can no longer join new projects.
- Admins grant the worker for each project. They add team `server` in the project, under **Settings -> Workers -> Grant Team**, then **Grant**.
- The API accepts `POST /projects/{project}/teams` together with `{"team": "server", "workers": true, "users": false}`.
- The state can take `services.gradient.state.projects.<name>.teams = [ { team = "server"; users = false; workers = true; } ]`.
- Set `services.gradient.grantLocalWorkerToNewProjects = true` to restore the previous behavior. Projects that had grants before the upgrade keep them.
- Only superusers create projects by default in installs that enable the local worker. Set `services.gradient.permissions.createProject = "everyone"` to restore the previous behavior.

### Registration

- Sign-up without an admin is off by default on every install. Set `services.gradient.registration.enable = true` (`GRADIENT_REGISTRATION_ENABLE`) to keep registration open.
- On an instance with no sign-up, declare the first superuser with `services.gradient.state.users.<name>.superuser = true` in the state.

### Guest Network

- Networked guests on one exec worker can no longer reach each other.
- Guests also lose private network (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `100.64.0.0/10`), link-local addresses including `169.254.169.254`, loopback, and all host ports. Guests can still use DNS on the bridge address while `exec.nameservers` is empty.
- Guests can still respond to connections from outside. This includes other nodes of a multi-node job on another worker.
- Guests can still reach the internet through NAT as before. IPv6 traffic from guests cannot pass without `exec.subnet.v6`, see [Guest IPv6](#guest-ipv6).
- Guests can't use another guest's address. The supervisor does reject a tap for addresses outside the range of `exec.subnet.v4` or for the network, broadcast and bridge addresses.
- Invalid `exec.subnet.v4` values stop the supervisor at startup.
- `exec.allow.allowedPrivateNetworks` and `exec.allow.guestConnectableHostTcpPorts` open a single target again. See [Limit the Guest Network](../exec/worker.md#guest-network). Multi-node jobs across workers need the allowlist on every worker.
- The module does load tables `bridge gradient-guests` and `inet gradient-guests` through unit `gradient-guest-nftables`. Both `nft` and `ip` are necessary for the supervisor. Both go on the supervisor's path through the module.
- Run `systemctl reload gradient-guest-nftables` after the manual `nft flush ruleset`. Running guests with a network connection have no filter until that point. New jobs that use a network connection fail.
- Guest tables remain after stopping `gradient-guest-nftables`. Both tables need manual deletion once exec is off.
- Reload, restart, stop, and start of `nftables.service` delete the guest tables when `networking.nftables.flushRuleset = true` is set. The worker module can add steps to `nftables.service` that restore the guest filter after a stop and after a start. systemd can reload or restart `gradient-guest-nftables` through reload or restart of `nftables.service`. Running guests lose the filter for a few milliseconds each time. Two such gaps occur during a restart.
- Root can remove the guest filter on the worker host with `nft flush ruleset`, manual deletion of guest tables, or a flush at `gradient-guest-nftables` shutdown. Guests keep unfiltered network access until the tables return. See [Filter Tables](../exec/worker.md#filter-tables).
- Guest supervisors keep one record per networked guest in `/run/gradient-guest-tap-addresses` (owner root, mode 0700). Don't edit these files.

### Worker and Guest Supervisor Order

- The worker and the guest supervisor use guest protocol 10 for the guest address in `PrepareTap`. Both come from the `gradient-worker` binary on one host.
- Guest images need no rebuild for protocol 10. Images only need the line in [Exec Work Disk](#exec-work-disk) about guest protocol 5.
- A protocol 9 or older worker can't send a guest address. New supervisor units reject the tap with the text below. Networked guests don't start.
- Restart `gradient-worker` during the upgrade. Workers start a new supervisor unit for each new guest.
- Networked guests from before the upgrade have no entry in `guest-tap-addresses`. The filter rules in `gradient-guest-nftables` drop traffic from those guests after the load. Stop running guests before the upgrade.

```text
tap "<name>" with no guest IPv4 address
```

### Guest IPv6

- `exec.subnet` is now an attribute set with `v4` and optional `v6` keys.

- The variable `GRADIENT_WORKER_EXEC_SUBNET` is now `GRADIENT_WORKER_EXEC_SUBNET_V4`. The new `GRADIENT_WORKER_EXEC_SUBNET_V6` is optional.
- The old `GRADIENT_WORKER_EXEC_SUBNET` variable can remain in the worker environment, for example through `environmentVariables`. Workers with that variable stop at startup with the error below.

```text
The worker won't start when the removed `GRADIENT_WORKER_EXEC_SUBNET` environment variable is set. Rename it to `GRADIENT_WORKER_EXEC_SUBNET_V4`.
```

- Two error texts change. The old texts `no free guest address in exec.subnet` and `two nodes have the guest address` no longer appear. Update alerts and log searches for them. The new texts are below.

```text
No free guest addresses in `exec.subnet.v4`
Two nodes have guest address <address>. Assign a unique exec.subnet.v4 to each exec worker.
```

- Workers do not start for v6 subnets without v4 subnet, with prefix length outside 64 to 96 plus v4's prefix length, or nonzero host bits.
- Guest images need a rebuild to get IPv6 from `exec.subnet.v6`. Old images keep working with IPv4 only. They have no IPv6 at all. Rules in the bridge filter drop those IPv6 frames. Workers write the line below to the job log once per job.

```text
The guest image is too old for IPv6. The protocol of the guest agent in this image is <version>. The first protocol with IPv6 support is 11. IPv4 is the only option for this job. Rebuild the guest image to provide IPv6 for the job.
```

- Set `exec.allow.blockedGlobalIPv6Networks` for `exec.subnet.v6`. Add the host's own LAN prefix to this list.

- `exec.subnet.v6` and `exec.allow.network` together enable IPv6 forwarding and masquerade host-wide via `networking.nat.enableIPv6`. All interfaces also get `accept_ra = 2` from the same option. Hosts with static IPv6 addressing now accept router advertisements that they ignored before. Hosts that get addresses via kernel SLAAC keep the default route only because of value 2.
- Hosts that obtain their own IPv6 address through kernel SLAAC need to check the default route after the switch.
- The `exec.allow.allowedPrivateNetworks` setting can also accept IPv6 networks. Those IPv6 entries only take effect on workers with the `exec.subnet.v6` option set.
- Guests with IPv6 can't reach IPv6 destinations outside `2000::/3` unless `allowedPrivateNetworks` entries include those destinations. Guests also can't reach `2001::/23`, `2001:db8::/32`, `2002::/16` and `3fff::/20`. Guests can't ping the IPv6 gateway. DNS queries use IPv4.
- Records in `/run/gradient-guest-tap-addresses` use a three-line format. The three lines are an IPv4 entry, an IPv6 entry or an empty line and a guest ID. Stop running guests with network connectivity before a worker upgrade, or reboot those guests with the upgrade. Guests that stay up across an upgrade keep an old-format record. The new release can't parse that record. Stops of other guests invoke cleanup. That cleanup can treat the record as residue and delete the tap and set entries of the running guest. Host reboots remove old records. Those records exist under `/run`. Scripts that load guest rules skip records that don't have three lines and write the line below to stderr.

```text
Skipped record <record>: it doesn't contain 3 lines
```
