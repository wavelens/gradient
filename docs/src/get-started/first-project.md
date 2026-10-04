# First Project

A flake built by Gradient, with the outputs in a binary cache.

**Requirements:**

- A running instance, see [Quick Start](quick-start.md) or [Standalone](standalone.md)
- A flake in a Git repository the server can reach

## 1. Register

Open `https://gradient.example.com/account/register` and create the first user.

The server will refuse registration with `registration.enable = false` or `oidc.required`. Sign in through OIDC instead, or declare the first user in [`services.gradient.state.users`](../reference/state.md#usersname).

## 2. Create a Cache

**Caches -> Create Cache**, then pick a name and a visibility.

Caches store every build output of the project. `nix` can fetch those outputs from the cache as a substituter.

## 3. Create a Project

**Projects -> Create Project**, then open **Settings -> Cache Subscriptions -> Subscribe to Cache** and pick the cache from step 2.

A project is the unit of access. Members, workers and caches belong to a project. The local worker will appear under **Settings -> Workers** within a minute of the subscription.

## 4. Create a Task

**Create Task** on the project page, then fill in these fields.

| Field | Value |
|---|---|
| Repository URL | The flake's Git URL, e.g. `https://github.com/wavelens/gradient` |
| Evaluation Wildcard | Which flake outputs to build, e.g. `packages.x86_64-linux.#`, see [wildcards](../reference/wildcards.md) |

A task is one repository plus the outputs to build from that repository.

!!! tip "Private Repositories"
    Every project has its own SSH key under **Settings -> SSH Key**. Add the public key as a deploy key on the Git host.

## 5. Start an Evaluation

**Start Evaluation** on the task page. Gradient will read the flake and find every derivation matching the wildcard. The builds then go to the worker.

## Verify Deployment

- The evaluation page shows every build with live logs, grouped by status.
- Every finished build has the status completed, with the outputs in the cache.

## Next Steps

- [Evaluation wildcards](../reference/wildcards.md): pick the outputs to build
- [Connect GitHub](../guides/github.md): evaluate on every push and pull request
- [Share a Cache](../guides/share-a-cache.md): use the cache from other machines
- [Add a Remote Worker](../guides/remote-worker.md): add build machines
