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

Public caches need nothing more. Private caches need an API key from **Settings -> API Keys** in a netrc file for the Nix daemon.

=== "CLI"

    ```sh
    sudo nix run github:wavelens/gradient/latest#gradient-cli -- cache install-netrc \
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

    In the other project, go to **Settings -> Cache Subscriptions -> Subscribe to Cache**.

    - With the Admin role on both sides, the subscription activate at once.
    - Any other subscription wait as a request, marked **Pending approval**. A cache admin can approve or deny requests under **Subscriptions** on the cache page.

=== "Declarative"

    ```nix
    services.gradient.state.caches.main.projects = [ "acme" "widgets" ];
    ```

    Declared subscriptions skip the approval step.

## 3. Invite Members

Roles on a cache belong to members of the cache itself. Project roles do not carry over.

=== "UI"

    On the cache page, open **Members & Roles -> Add Member**. Enter a user name and a role. The invited user accepts the invite on the **My Invites** page under **Settings**. Cache access only begins after acceptance.

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
- The member show up under **Members & Roles** on the cache page.

## Next Steps

- [Caches](../concepts/caches.md): how upstream caches, pull-through, and substitution order work
- [Members and Roles](../ui/members-and-roles.md#cache-roles): build custom roles with single permissions
- [Upload NARs](upload-nars.md): push paths that were built outside Gradient
