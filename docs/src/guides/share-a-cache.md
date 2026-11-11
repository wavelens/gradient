<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Share a Cache

One cache, used by machines, other projects and other people.

**Requirements:**

- A cache, see [First Project](../get-started/first-project.md)
- The Admin role on the cache, see [Caches](../concepts/caches.md#roles)

## 1. Use the Cache on a Machine

The cache page displays two values: the substituter URL and the public key.

```nix
nix.settings = {
  extra-substituters = [ "https://gradient.example.com/cache/main" ];
  extra-trusted-public-keys = [ "gradient.example.com-main:<public key>" ];
};
```

The one key can cover every path, even paths pulled through from an [upstream cache](../concepts/caches.md#pull-through). Gradient will verify these paths against the upstream cache's key. Gradient is then signing them again with the cache's own key.

Public caches need nothing else. Private caches need a netrc file for the Nix daemon. Store the API key in the netrc file. Create a key in **Settings -> API Keys**. Set the cache scope and select Read store as the permission. Keys with project scope read only the caches that the project subscribes to.

Use **Create key for this cache** on the cache page to open a key dialog with the cache scope and cache prefilled. Create a key. The netrc line, the nix.conf line, and the `install-netrc` command show the key filled in.

=== "CLI"

    ```sh
    sudo nix run nixpkgs#gradient-cli -- cache install-netrc \
      --server https://gradient.example.com --cache main --token <api key>
    ```

The command stores the entry in `/etc/nix/netrc`.

=== "Declarative"

    ```nix
    sops.secrets.gradient-api-key = { };
    sops.templates."nix-netrc" = {
      content = ''
        machine gradient.example.com
        login gradient
        password ${config.sops.placeholder.gradient-api-key}
      '';
      path = "/etc/nix/netrc"; # (1)!
    };
    ```

1.  The default `nix.settings.netrc-file`. Gradient can ignore the login field and read the API key from the password field.

## 2. Share with Another Project

A subscribed project both pushes build outputs to the cache and pulls packages from it.

=== "UI"

    Go to **Settings -> Cache Subscriptions -> Add Cache** in the other project. Select the cache and a mode (**Read & Write**, **Read only**, or **Write only**). The list can show for each cache whether the cache can accept writes or which cache admin can approve the request.

    - A subscription with write access to the cache can take effect at once.
    - All other subscriptions wait as requests marked **Pending approval** that carry the cache administrator name. The cache administrator can approve or deny the request under **Subscriptions** on the cache page or at **Requests** in the header user menu. The administrator can lower the requested mode at approval time, for example Read & Write to Read only.

    All subscription rows show a switch **Sign pushed paths with the cache key**. The paths from the project to the cache receive a signature from the server while the switch is on. In the CLI, choose the mode with `gradient project cache add <CACHE> --mode ReadOnly`.

=== "Declarative"

    ```nix
    services.gradient.state.caches.main.projects = [
      "acme"
      { name = "widgets"; mode = "ReadOnly"; sign_outputs = false; }
    ];
    ```

    Declared subscriptions skip the approval step. The UI and API lock them. Change or remove them in the config. The default values are `ReadWrite` for `mode` and `true` for `sign_outputs`.

## 3. Invite Members

Roles on a cache belong to members of the cache itself. Project roles do not carry over.

=== "UI"

    On the cache page, open **Members -> Invite Member**. Enter a user name and a role, then pick **Send invitation**. The invited user accepts the invitation on the **Requests** page in the user menu of the header. Cache access only begins after acceptance. Instance administrators and people adding a user who already has access through a team can pick **Add now** instead.

    - Invites expire after 7 days.
    - With [mail](../reference/configuration.md#email) configured, the invitee also gets an email with a link.

=== "Declarative"

    ```nix
    services.gradient.state.caches.main.members = [
      { user = "alice"; role = "Write"; }
      { user = "bob"; role = "View"; }
    ];
    ```

    Members declared this way skip the invitation flow.

## Verify Deployment

```sh
nix store info --store https://gradient.example.com/cache/main
```

- The output shows the store info of the cache.
- A private cache return `401` without a valid netrc entry.
- The other project list the cache under **Settings -> Cache Subscriptions** with no pending badge.
- Members appear under **Members** on the cache page.

## Next Steps

- [Caches](../concepts/caches.md): how upstream caches, pull-through, and substitution order work
- [Members and Roles](../ui/members-and-roles.md#cache-roles): build custom roles with single permissions
- [Upload NARs](upload-nars.md): push paths that were built outside Gradient
