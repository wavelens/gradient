<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# First Project

A flake built by Gradient, with the outputs in a binary cache.

**Requirements:**

- A running instance. Setup options: [Quick Start](quick-start.md) or [Standalone](standalone.md)
- A flake inside a Git repository that the server can access

## 1. First User

=== "UI"

    Open `https://gradient.example.com/account/register` and create the first user.

    The server will refuse registration with `registration.enable = false` or `oidc.required`. Use the declarative tab or sign in through OIDC then.

=== "Declarative"

    ```sh
    install -d -m 700 /var/lib/gradient-secrets
    gradient hash > /var/lib/gradient-secrets/admin-password # (1)!
    ```

    1.  Password prompt twice, then an argon2id hash as output. The [CLI](../reference/cli.md#install) page lists the install steps.

    ```nix
    services.gradient.state.users.admin = {
      email = "admin@example.com";
      password_file = "/var/lib/gradient-secrets/admin-password"; # (1)!
      superuser = true;
    };
    ```

    1.  The file contains the hash, not the password. A plaintext password in it would make the server fail on start.

    Sign in as `admin` with the password. Registration can stay off. Other user options are in [Declarative State](../concepts/declarative-state.md).

## 2. Create a Cache

Go to **Caches -> Create Cache**. Choose a name and set the visibility.

A cache holds all build outputs of the project. `nix` treats the cache as a substituter and can pull those outputs from it.

## 3. Create a Project

Open **Projects -> Create Project** and set up a new project. Then go to **Settings -> Cache Subscriptions -> Subscribe to Cache** and select the cache created in step 2.

A project acts as the access boundary. All members, workers and caches live inside one project. After the subscription, the local worker shows up under **Settings -> Workers** within about a minute.

## 4. Create a Task

A task combines one repository with the flake outputs to build from it.

Open the project page and select **Create Task**. Then fill in the two fields in the form.

| Field | Value |
|---|---|
| Repository URL | Git URL of the flake, e.g. `https://github.com/wavelens/gradient` |
| Evaluation Wildcard | Flake outputs to build, e.g. `packages.x86_64-linux.*`, see [wildcards](../reference/wildcards.md) |

!!! tip "Private Repositories"
    Each project ships with an SSH key, listed under **Settings -> SSH Key**. Add the public key as a deploy key on the Git host to access private repositories.

## 5. Start an Evaluation

Press **Start Evaluation** on the task page. Gradient will then scan the flake and locate every derivation that fit the wildcard. All those builds go to the worker.

## Verify Deployment

- The evaluation page lists all builds with live logs, sorted by status.
- Completed builds show the status `completed` and store their outputs in the cache.

## Next Steps

- [Evaluation wildcards](../reference/wildcards.md): choose which outputs to build
- [Connect GitHub](../guides/github.md): run evaluations on every push and pull request
- [Share a Cache](../guides/share-a-cache.md): reuse the cache across other machines
- [Add a Remote Worker](../guides/remote-worker.md): attach extra build machines
