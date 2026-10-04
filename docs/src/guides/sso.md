# Set Up Single Sign-On

Sign-in through the company identity provider (Keycloak, Kanidm, Authentik, Okta, Entra ID), with team memberships from the provider's groups.

**Requirements:**

- An identity provider with OpenID Connect
- SCIM support in the provider (Okta, Entra ID), for provisioning only

## 1. Register Gradient in the Provider

Create an OIDC client with the settings below.

| Setting | Value |
|---|---|
| Redirect URL | `https://gradient.example.com/api/v1/auth/oidc/callback` |
| Scopes | `openid`, `email`, `profile`, plus `groups` for team mapping |
| PKCE | `S256`. Gradient is always sending PKCE |

Store the client secret as a file on the server.

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

1.  Gradient will read every endpoint from `<discoveryUrl>/.well-known/openid-configuration`.
2.  `groups` is not in the default scopes. Add `groups` for [team mapping](#3-map-groups-to-teams) where the provider can offer the scope.
3.  Hiding the username and password login. Leave out to offer both.

## 3. Map Groups to Teams

A [team](../concepts/teams.md) can follow one provider group. Grants of the team then decide the projects and caches of its members.

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

Members of `acme-eng` join the team on each sign-in. Users without `acme-eng` in the claim leave the team on the next sign-in. Members added by hand stay.

## 4. Provision with SCIM

Optional. The provider can create, update and disable Gradient accounts before the first sign-in. Group changes apply at once.

```nix
services.gradient.scim = {
  enable = true;
  tokenFile = "/run/secrets/gradient-scim-token"; # (1)!
};

services.gradient.state.teams.acme-eng.scim_group = "acme-eng";
```

1.  Any random string, e.g. `openssl rand -hex 32`. The provider must send the same value as bearer token.

Set the SCIM base URL `https://gradient.example.com/scim/v2` and the bearer token in the provider. Enable pushing users, profile updates and groups.

| Provider action | Effect in Gradient |
|---|---|
| Add a user | A new passwordless account, claimed on the first OIDC sign-in |
| Add to a group | Member of the team with that `scim_group` |
| Remove from a group | Team membership removed |
| Deactivate or delete | Sign-in blocked, history kept. `scim.hardDelete = true` is deleting the account |

## Verify Deployment

- The login page will show the provider's button.
- A sign-in will land on the dashboard.
- The user is now listed on the `acme-eng` team page, with the **Group** badge.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `An account already exists with this username or email` | A password account is holding the same name or email. Delete or rename the account, or sign in with the password |
| No team after sign-in | The `groups` scope is missing, or the group name does not match the team's `oidc_group` |
| `account is deactivated` | SCIM deactivated the account in the provider |
| SCIM calls return `404` for a group | No team has the group as `scim_group` |

## Next Steps

- [Teams](../concepts/teams.md): grants, roles and team workers
- [Declarative State](../concepts/declarative-state.md): teams, projects and members as NixOS options
- [Configuration](../reference/configuration.md#oidc): every OIDC and SCIM option
