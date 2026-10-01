# Share a Cache

One cache, used by machines, other projects and other people.

**Requirements:**

- A cache, see [First Project](../get-started/first-project.md)
- The Admin role on the cache, see [Caches](../concepts/caches.md#roles)

## 1. Use the Cache on a Machine

The cache page shows the substituter URL and the public key:

```nix
nix.settings = {
  extra-substituters = [ "https://gradient.example.com/cache/main" ];
  extra-trusted-public-keys = [ "gradient.example.com-main:<public key>" ];
};
```

The one key covers every path, even paths pulled through from an [upstream cache](../concepts/caches.md#pull-through): Gradient verifies them against the upstream cache's key and signs them again with the cache's own.

A public cache needs nothing more. A private cache needs an API key from **Settings -> API Keys** in a netrc file for the Nix daemon:

=== "CLI"

    ```sh
    sudo nix run github:wavelens/gradient#gradient-cli -- cache install-netrc \
      --server https://gradient.example.com --cache main --token <api key>
    ```

    The command writes the entry to `/etc/nix/netrc`.

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

    1.  The default `nix.settings.netrc-file`; Gradient ignores the login and reads the password as the API key.

## 2. Share with Another Project

A subscribed project pushes its outputs to the cache and substitutes from the cache.

=== "UI"

    In the other project, **Settings -> Cache Subscriptions -> Subscribe to Cache**.

    - With the Admin role on both sides, the subscription is active at once.
    - Otherwise the subscription waits as a request, marked **Pending approval**. A cache admin approves or denies the request under **Subscriptions** on the cache page.

=== "Declarative"

    ```nix
    services.gradient.state.caches.main.projects = [ "acme" "widgets" ];
    ```

    Declared subscriptions skip the approval.

## 3. Invite Members

Members get a role on the cache itself, independent of any project.

=== "UI"

    On the cache page, **Members & Roles -> Add Member**, with a user name and a role. The invitee accepts under **Settings -> My Invites**; until then the invitee has no access.

    - An invitation expires after 7 days.
    - With [mail](../reference/configuration.md#email) configured, the invitee also gets a mail with a link.

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

- Prints the cache's store info; a private cache answers `401` without a valid netrc entry.
- The other project lists the cache under **Settings -> Cache Subscriptions** without a pending badge.
- The member shows under **Members & Roles** on the cache page.

## Next Steps

- [Caches](../concepts/caches.md): upstream caches, pull-through and substitution order
- [Members and Roles](../ui/members-and-roles.md#cache-roles): custom roles with single permissions
- [Upload NARs](upload-nars.md): push paths built outside Gradient
