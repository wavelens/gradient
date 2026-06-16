# Cache roles & permissions

## Overview

Caches mirror the organization permission model: each cache carries its own
set of per-user roles with bitmask-encoded capabilities. Three immutable
built-in roles (Admin, Write, View) cover the common cases; members with the
`manageCacheRoles` capability can add custom roles scoped to a single cache.
Access to a cache route is resolved the same way as org access-the caller's
role bitmask is intersected with the required capability and a 403 is returned
if the bit is absent.

## Capability table

| Capability | Description |
|---|---|
| `viewCache` | See the cache in lists and view its metadata. |
| `readStore` | Download NARs from the cache. |
| `writeStore` | Upload NARs to the cache. |
| `manageCacheSettings` | Edit name, priority, public/active flags. |
| `manageCacheKeys` | View and rotate signing keys. |
| `manageCacheUpstreams` | Add, edit, and remove upstream caches. |
| `manageCacheMembers` | Add, update, and remove members. |
| `manageCacheRoles` | Create, edit, and delete custom roles. |
| `manageCacheSubscriptions` | Approve and revoke org subscriptions to this cache. |
| `deleteCache` | Delete the cache. |

## Built-in roles

Every cache carries three immutable system roles:

| Role | Capabilities |
|---|---|
| Admin | All ten capabilities. |
| Write | `viewCache`, `readStore`, `writeStore` |
| View | `viewCache`, `readStore` |

Built-in roles cannot be edited or deleted.

## Member management

Members are managed via the `/api/v1/caches/{cache}/members` family:

| Method | Path | Description |
|---|---|---|
| `GET` | `/caches/{cache}/members` | List members with their role. |
| `POST` | `/caches/{cache}/members` | Add a member (requires `manageCacheMembers`). |
| `PATCH` | `/caches/{cache}/members/{user}` | Change a member's role (requires `manageCacheMembers`). |
| `DELETE` | `/caches/{cache}/members/{user}` | Remove a member (requires `manageCacheMembers`). |

The last Admin member of a cache cannot be removed; the caller must promote
another member to Admin first.

## Custom roles

Custom roles are managed via the `/api/v1/caches/{cache}/roles` family:

| Method | Path | Description |
|---|---|---|
| `GET` | `/caches/{cache}/roles` | List roles plus the `available_permissions` catalogue. |
| `POST` | `/caches/{cache}/roles` | Create a custom role (requires `manageCacheRoles`). |
| `PATCH` | `/caches/{cache}/roles/{role}` | Update a custom role's name or capabilities (requires `manageCacheRoles`). |
| `DELETE` | `/caches/{cache}/roles/{role}` | Delete a custom role (requires `manageCacheRoles`). |

Built-in roles are immutable and are rejected with 403 on PATCH or DELETE.
Roles declared in `gradient-state.nix` are also immutable via the API (see
below).

## Bilateral subscription gate

> **Note:** Subscribing an organization to a cache requires
> `manageSubscriptions` on the **org** AND `manageCacheSubscriptions` on the
> **cache**. Public caches no longer bypass this gate.

Both permissions must be held simultaneously. A caller who has
`manageCacheSubscriptions` on a cache but lacks `manageSubscriptions` for the
subscribing org (or vice versa) receives 403.

## State-managed caches

When a cache, its members, or its roles are declared in `gradient-state.nix`,
the corresponding rows are **immutable via the API**: mutating requests return
403. This mirrors the managed-resource guard for organizations and projects.
NAR-content endpoints (upload, download) are still permitted on managed caches;
only configuration-level writes are blocked.
