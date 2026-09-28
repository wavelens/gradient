# First Project

A flake built by Gradient, with the outputs in a binary cache.

**Requirements:**

- A running instance, see [Quick Start](quick-start.md)
- A flake in a Git repository the server can reach

## 1. Register

Open `https://gradient.example.com/account/register` and create the first user.

With `registration.enable = false` or `oidc.required`, the server refuses registration: sign in through OIDC, or declare the first user in [`services.gradient.state.users`](../reference/state.md#usersname) instead.

## 2. Create a Cache

**Caches -> Create Cache**, then pick a name and a visibility.

A cache stores every output the project builds and serves them to `nix` as a substituter.

## 3. Create a Project

**Projects -> Create Project**, then open **Settings -> Cache Subscriptions -> Subscribe to Cache** and pick the cache from step 2.

A project is the unit of access: members, workers and caches belong to a project. The local worker shows up under **Settings -> Workers** within a minute of the subscription.

## 4. Create a Task

On the project page, **Create Task**:

| Field | Value |
|---|---|
| Repository URL | The flake's Git URL, e.g. `https://github.com/wavelens/gradient` |
| Evaluation Wildcard | Which flake outputs to build, e.g. `packages.x86_64-linux.#`, see [wildcards](../reference/wildcards.md) |

A task is one repository plus the outputs to build from that repository.

!!! tip "Private Repositories"
    Each project has its own SSH key under **Settings -> SSH Key**. Add the public key as a deploy key on the Git host.

## 5. Start an Evaluation

**Start Evaluation** on the task page. Gradient reads the flake, finds every derivation the wildcard selects and hands the builds to the worker.

## Verify Deployment

- The evaluation page lists every build, grouped by status, with live logs.
- Finished builds show as completed, and the outputs are in the cache.

## Next Steps

- [Evaluation wildcards](../reference/wildcards.md): select exactly the outputs to build
- [Connect GitHub](../guides/forge-github.md): evaluate on every push and pull request
- [Share a Cache](../guides/share-a-cache.md): use the cache from other machines
- [Add a Remote Worker](../guides/remote-worker.md): add build machines
