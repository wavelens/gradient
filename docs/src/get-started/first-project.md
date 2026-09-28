# First project

A flake built by Gradient, with the outputs in a binary cache.

**Requirements:**

- A running instance, see [Quick start](quick-start.md)
- A flake in a Git repository the server can reach

## 1. Register

Open `https://gradient.example.com/account/register` and create the first user.

## 2. Create a cache

**Caches -> Create Cache**, then pick a name and a visibility.

A cache stores every output the project builds and serves them to `nix` as a substituter.

## 3. Create a project

**Projects -> Create Project**, then open **Settings -> Cache Subscriptions -> Subscribe to Cache** and pick the cache from step 2.

A project is the unit of access: members, workers and caches belong to a project. The local worker shows up under **Settings -> Workers** within a minute of the subscription.

## 4. Create a task

On the project page, **Create Task**:

| Field | Value |
|---|---|
| Repository URL | The flake's Git URL, e.g. `https://github.com/wavelens/gradient` |
| Evaluation Wildcard | Which flake outputs to build, default `packages.x86_64-linux.*`, see [wildcards](../usage/overview.md#evaluation-wildcard) |

A task is one repository plus the outputs to build from that repository.

!!! tip "Private repositories"
    Each project has its own SSH key under **Settings -> SSH**. Add the public key as a deploy key on the Git host.

## 5. Start an evaluation

**Start Evaluation** on the task page. Gradient reads the flake, finds every derivation the wildcard selects and hands the builds to the worker.

## Verify Deployment

- The evaluation page lists every build, grouped by status, with live logs.
- Finished builds show as completed, and the outputs are in the cache.

## Next steps

- [Evaluation wildcards](../usage/overview.md#evaluation-wildcard): select exactly the outputs to build
- [Forge integration](../usage/integration.md): evaluate on every push and pull request
- [Caches](../usage/caches.md): use the cache from other machines
- [Add a Remote Worker](../guides/remote-worker.md): add build machines
