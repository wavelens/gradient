# Set Up Single Sign-On

Sign-in through the company identity provider (Keycloak, Kanidm, Authentik, Okta, Entra ID), with project roles from the provider's groups.

**Requirements:**

- An identity provider with OpenID Connect
- For provisioning: SCIM support in the provider (Okta, Entra ID)

## 1. Register Gradient in the Provider

Create an OIDC client with:

| Setting | Value |
|---|---|
| Redirect URL | `https://gradient.example.com/api/v1/auth/oidc/callback` |
| Scopes | `openid`, `email`, `profile`, plus `groups` for role mapping |
| PKCE | `S256`; Gradient always sends PKCE |

Store the client secret as a file on the server.

## 2. Enable OIDC

```nix
services.gradient.oidc = {
  enable = true;
  discoveryUrl = "https://auth.example.com"; # (1)!
  clientId = "gradient";
  clientSecretFile = "/run/secrets/gradient-oidc-secret";
  scopes = [ "openid" "email" "profile" "groups" ];
  required = true; # (2)!
};
```

1.  Gradient reads every endpoint from `<discoveryUrl>/.well-known/openid-configuration`.
2.  Hides the username and password login; leave out to offer both.

## 3. Map Groups to Roles

A custom role lists the provider groups that grant the role:

```nix
services.gradient.state.roles.acme-engineer = {
  project = "acme";
  permissions = [ "viewProject" "triggerEvaluation" ];
  oidc_group = [ "acme-eng" ];
};
```

On each sign-in, a member of `acme-eng` gets the `acme-engineer` role in `acme`. Groups only add roles; leaving a group removes nothing until SCIM or an admin does.

## 4. Provision with SCIM

Optional. The provider creates, updates and disables Gradient accounts before anyone signs in, and group changes apply at once.

```nix
services.gradient.scim = {
  enable = true;
  tokenFile = "/run/secrets/gradient-scim-token"; # (1)!
};

services.gradient.state.roles.acme-engineer.scim_group = [ "acme-eng" ];
```

1.  Any random string, e.g. `openssl rand -hex 32`; the provider sends the same value as bearer token.

In the provider, set the SCIM base URL `https://gradient.example.com/scim/v2`, the bearer token, and enable pushing users, profile updates and groups.

| Provider action | Effect in Gradient |
|---|---|
| Add a user | Creates a passwordless account, claimed on the first OIDC sign-in |
| Add to a group | Grants every role with that `scim_group` |
| Remove from a group | Revokes those roles |
| Deactivate or delete | Blocks sign-in and keeps the history; `scim.hardDelete = true` deletes the account |

## Verify Deployment

- The login page shows the provider's button; signing in lands on the dashboard.
- **Members & Roles** in `acme` lists the user with the `acme-engineer` role.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `An account already exists with this username or email` | A password account holds the same name or email; delete or rename the account, or sign in with the password |
| No roles after sign-in | The `groups` scope is missing, or the group name differs from `oidc_group` |
| `account is deactivated` | SCIM deactivated the account in the provider |
| SCIM calls return `404` for a group | No role lists the group in `scim_group` |

## Next Steps

- [Declarative State](../concepts/declarative-state.md): roles, projects and members as NixOS options
- [Configuration](../configuration.md#oidc): every OIDC and SCIM option
