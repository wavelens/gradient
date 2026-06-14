# SCIM

Gradient exposes an instance-level SCIM 2.0 (RFC 7643/7644) provisioning surface
so an identity provider (Okta, Microsoft Entra ID, ...) can create, update, and
deprovision Gradient users and manage role membership automatically.

SCIM handles **provisioning only**. Provisioned users are passwordless `managed`
accounts; they sign in through [OIDC](../configuration.md#oidc), claiming their
account on first login. Run SCIM and OIDC against the same IdP so provisioning and
authentication line up.

## Enabling SCIM

```nix
services.gradient.scim = {
  enable     = true;
  tokenFile  = "/run/secrets/gradient-scim-token";
  hardDelete = false;
};
```

When enabled, the endpoints mount at `https://$domain/scim/v2`. Every request is
authenticated by the bearer token in `tokenFile` (not user credentials), so keep
the token secret and rotate it like any other credential. When SCIM is disabled,
`/scim/v2` is not mounted at all.

## Groups → roles

A SCIM group is an IdP group name, not a Gradient object: groups are not created
through SCIM. Each group resolves to `(organization, role)` grants via the
`scim_group` list on a state-managed role:

```nix
services.gradient.state.roles.acme-engineer = {
  organization = "acme";
  permissions  = [ "viewOrg" "triggerEvaluation" ];
  scim_group   = [ "acme-eng" ];
};
```

Adding a user to the `acme-eng` SCIM group grants the `acme-engineer` role in the
`acme` organization; removing them revokes it. Grants are additive across groups.
A group name with no matching `scim_group` entry is unknown and returns `404`.
See [Declarative State](state.md).

## Deprovisioning

`DELETE /Users/{id}` (and `active=false`) **soft-disables** by default: the user
is marked inactive and can no longer log in (`403`), but the row and its history
remain. Set `hardDelete = true` to cascade-delete on `DELETE` instead.

## Okta / Entra setup

Configure SCIM provisioning in the IdP with:

- **Base URL:** `https://<host>/scim/v2`
- **Authentication:** HTTP Header / OAuth bearer token = the contents of `tokenFile`
- Enable Push New Users, Push Profile Updates, and Push Groups; map IdP groups to
  the names you listed in `scim_group`.
