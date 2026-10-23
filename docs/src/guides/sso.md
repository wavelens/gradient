<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Set Up Single Sign-On

Sign-in through the company identity provider (Keycloak, Kanidm, Authentik, Okta, Entra ID), with team memberships from the provider's groups.

**Requirements:**

- An identity provider with OpenID Connect
- SCIM support in the provider (Okta, Entra ID), for provisioning only

## 1. Register Gradient in the Provider

Set up an OIDC client on the provider side. Gradient needs the following configuration.

| Setting | Value |
|---|---|
| Redirect URL | `https://gradient.example.com/api/v1/auth/oidc/callback` |
| Scopes | `openid`, `email`, `profile`, plus `groups` for team mapping |
| PKCE | `S256`. Gradient always sends PKCE |

The client secret belongs in a file on the server.

## 2. Enable OIDC

```nix
services.gradient.oidc = {
  enable = true;
  discoveryUrl = "https://auth.example.com"; # (1)!
  clientId = "gradient";
  clientSecretFile = "/run/secrets/gradient-oidc-secret";
  scopes = [ "openid" "email" "profile" "groups" ]; # (2)!
  required = true; # (3)!
};
```

1. All provider endpoints come from `<discoveryUrl>/.well-known/openid-configuration`.
2. The `groups` scope is off by default. Providers that support it need `groups` here for [team mapping](#3-map-groups-to-teams).
3. With `required = true`, the username and password form disappear. Omit it to keep both login methods.

## 3. Map Groups to Teams

Each [team](../concepts/teams.md) can follow one provider group. Team grants then control which projects and caches its members can access.

=== "UI"

    Superusers can set **OIDC group** on the team's **Settings** page.

=== "Declarative"

    ```nix
    services.gradient.state.teams.acme-eng = {
      display_name = "ACME Engineering";
      oidc_group = "acme-eng";
    };
    services.gradient.state.projects.acme.teams = [
      { team = "acme-eng"; role = "Write"; workers = false; }
    ];
    ```

Users with `acme-eng` in their claim join the team at sign-in. Users without that group in the claim leave the team at their next sign-in. Hand-added members stay.

## 4. Provision with SCIM

SCIM provisioning is optional. The identity provider can create, update and disable Gradient accounts before anyone signs in for the first time. Group changes take effect immediately.

```nix
services.gradient.scim = {
  enable = true;
  tokenFile = "/run/secrets/gradient-scim-token"; # (1)!
};

services.gradient.state.teams.acme-eng.scim_group = "acme-eng";
```

1.  Any random string, e.g. `openssl rand -hex 32`. The provider sends this value as the bearer token.

Configure the provider with the SCIM base URL `https://gradient.example.com/scim/v2` and the bearer token. Turn on pushing of users, profile updates and groups.

| Provider action | Effect in Gradient |
|---|---|
| Add a user | A new passwordless account, claimed on the first OIDC sign-in |
| Add to a group | Member of the team with that `scim_group` |
| Remove from a group | Team membership removed |
| Deactivate or delete | Sign-in blocked, history kept. `scim.hardDelete = true` deletes the account |

## Verify Deployment

- The provider button appears on the login page.
- A successful sign-in lands on the dashboard.
- The new user shows up on the `acme-eng` team page with the **Group** badge.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `An account already exists with this username or email` | A password account already holds that name or email. Delete the account, rename it, or sign in with the password instead |
| No team after sign-in | The `groups` scope is missing from the request, or the group name does not match the team's `oidc_group` |
| `account is deactivated` | SCIM deactivated this account at the provider |
| SCIM calls return `404` for a group | No team lists that group as its `scim_group` |

## Next Steps

- [Teams](../concepts/teams.md) - grants, roles and team workers
- [Declarative State](../concepts/declarative-state.md) - teams, projects and members as NixOS options
- [Configuration](../reference/configuration.md#oidc) - all OIDC and SCIM options
