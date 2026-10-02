# Set Up Single Sign-On

Sign-in through the company identity provider (Keycloak, Kanidm, Authentik, Okta, Entra ID), with project roles from the provider's groups.

**Requirements:**

- An identity provider with OpenID Connect
- SCIM support in the provider (Okta, Entra ID), for provisioning only

## 1. Register Gradient in the Provider

Create an OIDC client with the settings below.

| Setting | Value |
|---|---|
| Redirect URL | `https://gradient.example.com/api/v1/auth/oidc/callback` |
| Scopes | `openid`, `email`, `profile`, plus `groups` for role mapping |
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

1.  Gradient is reading every endpoint from `<discoveryUrl>/.well-known/openid-configuration`.
2.  `groups` is not in the default scopes. Add `groups` for [role mapping](#3-map-groups-to-roles) where the provider is supporting the scope.
3.  Hiding the username and password login. Leave out to offer both.

## 3. Map Groups to Roles

A custom role is listing the provider groups that grant the role.

```nix
services.gradient.state.roles.acme-engineer = {
  project = "acme";
  permissions = [ "viewProject" "triggerEvaluation" ];
  oidc_group = [ "acme-eng" ];
};
```

A member of `acme-eng` is receiving the `acme-engineer` role in `acme` on each sign-in. Groups only add roles. A member leaving a group keeps every role. Only SCIM or an admin can take a role away.

## 4. Provision with SCIM

Optional. The provider is creating, updating and disabling Gradient accounts before the first sign-in. Group changes apply at once.

```nix
services.gradient.scim = {
  enable = true;
  tokenFile = "/run/secrets/gradient-scim-token"; # (1)!
};

services.gradient.state.roles.acme-engineer.scim_group = [ "acme-eng" ];
```

1.  Any random string, e.g. `openssl rand -hex 32`. The provider is sending the same value as bearer token.

Set the SCIM base URL `https://gradient.example.com/scim/v2` and the bearer token in the provider. Enable pushing users, profile updates and groups.

| Provider action | Effect in Gradient |
|---|---|
| Add a user | A new passwordless account, claimed on the first OIDC sign-in |
| Add to a group | Every role with that `scim_group` granted |
| Remove from a group | Those roles revoked |
| Deactivate or delete | Sign-in blocked, history kept. `scim.hardDelete = true` is deleting the account |

## Verify Deployment

- The login page is showing the provider's button.
- Signing in is landing on the dashboard.
- **Members & Roles** in `acme` is listing the user with the `acme-engineer` role.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `An account already exists with this username or email` | A password account is holding the same name or email. Delete or rename the account, or sign in with the password |
| No roles after sign-in | The `groups` scope is missing, or the group name does not match `oidc_group` |
| `account is deactivated` | SCIM deactivated the account in the provider |
| SCIM calls return `404` for a group | No role is listing the group in `scim_group` |

## Next Steps

- [Declarative State](../concepts/declarative-state.md): roles, projects and members as NixOS options
- [Configuration](../reference/configuration.md#oidc): every OIDC and SCIM option
