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

The cache page will show the substituter URL and the public key.

```nix
nix.settings = {
  extra-substituters = [ "https://gradient.example.com/cache/main" ];
  extra-trusted-public-keys = [ "gradient.example.com-main:<public key>" ];
};
```

The one key can cover every path, even paths pulled through from an [upstream cache](../concepts/caches.md#pull-through). Gradient will verify these paths against the upstream cache's key. Gradient is then signing them again with the cache's own key.

Public caches need nothing more. Private caches need an API key from **Settings -> API Keys** in a netrc file for the Nix daemon.

=== "CLI"

    ```sh
    sudo nix run github:wavelens/gradient/latest#gradient-cli -- cache install-netrc \
      --server https://gradient.example.com --cache main --token <api key>
    ```

    The command will write the entry to `/etc/nix/netrc`.

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

    1.  The default `nix.settings.netrc-file`. Gradient will ignore the login and read the password as the API key.

## 2. Share with Another Project

Subscribed projects push their outputs to the cache. The project is also substituting from the cache.

=== "UI"

    Open **Settings -> Cache Subscriptions -> Subscribe to Cache** in the other project.

    - The subscription is active at once with the Admin role on both sides.
    - Any other subscription will wait as a request, marked **Pending approval**. A cache admin can approve or deny the request under **Subscriptions** on the cache page.

=== "Declarative"

    ```nix
    services.gradient.state.caches.main.projects = [ "acme" "widgets" ];
    ```

    Declared subscriptions skip the approval.

## 3. Invite Members

Members get a role on the cache itself, independent of any project.

=== "UI"

    Open **Members & Roles -> Add Member** on the cache page. Enter a user name and a role. The invitee can accept on the **My Invites** page under **Settings**. Access is granted only after accepting.

    - Invitations expire after 7 days.
    - The invitee will also receive a mail with a link once [mail](../reference/configuration.md#email) is configured.

=== "Declarative"

    ```nix
    services.gradient.state.caches.main.members = [
      { user = "alice"; role = "Write"; }
      { user = "bob"; role = "View"; }
    ];
    ```

    Declared members skip the invitation.

## Verify Deployment

```sh
nix store info --store https://gradient.example.com/cache/main
```

- The command will print the cache's store info.
- A private cache will answer `401` without a valid netrc entry.
- The other project will list the cache under **Settings -> Cache Subscriptions** without a pending badge.
- The member is listed under **Members & Roles** on the cache page.

## Next Steps

- [Caches](../concepts/caches.md): upstream caches, pull-through and substitution order
- [Members and Roles](../ui/members-and-roles.md#cache-roles): custom roles with single permissions
- [Upload NARs](upload-nars.md): push paths built outside Gradient
