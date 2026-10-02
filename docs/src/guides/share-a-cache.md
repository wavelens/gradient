# Share a Cache

One cache, used by machines, other projects and other people.

**Requirements:**

- A cache, see [First Project](../get-started/first-project.md)
- The Admin role on the cache, see [Caches](../concepts/caches.md#roles)

## 1. Use the Cache on a Machine

The cache page is showing the substituter URL and the public key.

```nix
nix.settings = {
  extra-substituters = [ "https://gradient.example.com/cache/main" ];
  extra-trusted-public-keys = [ "gradient.example.com-main:<public key>" ];
};
```

The one key is covering every path, even paths pulled through from an [upstream cache](../concepts/caches.md#pull-through). Gradient is verifying these paths against the upstream cache's key. Gradient is then signing them again with the cache's own key.

Public caches need nothing more. Private caches need an API key from **Settings -> API Keys** in a netrc file for the Nix daemon.

=== "CLI"

    ```sh
    sudo nix run github:wavelens/gradient#gradient-cli -- cache install-netrc \
      --server https://gradient.example.com --cache main --token <api key>
    ```

    The command is writing the entry to `/etc/nix/netrc`.

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

    1.  The default `nix.settings.netrc-file`. Gradient is ignoring the login and reading the password as the API key.

## 2. Share with Another Project

A subscribed project is pushing its outputs to the cache. The project is also substituting from the cache.

=== "UI"

    Open **Settings -> Cache Subscriptions -> Subscribe to Cache** in the other project.

    - The subscription is active at once with the Admin role on both sides.
    - Any other subscription is waiting as a request, marked **Pending approval**. A cache admin can approve or deny the request under **Subscriptions** on the cache page.

=== "Declarative"

    ```nix
    services.gradient.state.caches.main.projects = [ "acme" "widgets" ];
    ```

    Declared subscriptions skip the approval.

## 3. Invite Members

Members get a role on the cache itself, independent of any project.

=== "UI"

    Open **Members & Roles -> Add Member** on the cache page. Enter a user name and a role. The invitee can accept under **Settings -> My Invites**. Access is granted only after accepting.

    - Invitations expire after 7 days.
    - The invitee is also receiving a mail with a link once [mail](../reference/configuration.md#email) is configured.

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

- The command is printing the cache's store info.
- A private cache is answering `401` without a valid netrc entry.
- The other project is listing the cache under **Settings -> Cache Subscriptions** without a pending badge.
- The member is listed under **Members & Roles** on the cache page.

## Next Steps

- [Caches](../concepts/caches.md): upstream caches, pull-through and substitution order
- [Members and Roles](../ui/members-and-roles.md#cache-roles): custom roles with single permissions
- [Upload NARs](upload-nars.md): push paths built outside Gradient
