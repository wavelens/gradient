Gradient is a Nix-based Continuous Integration system. It evaluates Nix flakes,
schedules derivation builds across remote machines, and exposes build artefacts
through an integrated binary cache.

## Authentication

Most endpoints require authentication. Three methods are accepted:

| Method | How to send | How to obtain |
|---|---|---|
| JWT (cookie) | `jwt_token` httpOnly cookie | Set automatically on login |
| JWT (header) | `Authorization: Bearer <token>` | `POST /auth/basic/login` or OIDC flow |
| API Key | `Authorization: Bearer GRAD<key>` | `POST /user/keys` |

Cookie auth is the default for browser clients - the `jwt_token` cookie is set
by the login and OIDC-callback endpoints and cleared on logout. The
`Authorization` header is accepted as a fallback (e.g. for CI/scripts).

### Artefact downloads

`GET /builds/{build}/download/{filename}` serves build artefacts declared in
`nix-support/hydra-build-products`. Access is granted when any of the following
conditions is met:

- The owning project is **public** - no credentials required.
- A short-lived (1 h) **download token** is supplied via `?token=<download_token>`.
  Obtain one from `GET /builds/{build}/download-token` while authenticated.
  Tokens are scoped to the build's derivation and are safe to embed in URLs.
- A valid session (`jwt_token` cookie or `Authorization: Bearer` header) belonging
  to a member of the owning project, or of any project with a build of the
  same derivation (the rule every other build read uses).

## Rate limiting

All HTTP endpoints enforce a per-client-IP token-bucket rate limit. The
limiter keys solely on client IP, so authenticated and anonymous requests
share the same tier. Clients that exceed their bucket receive HTTP
`429 Too Many Requests`. The tier depends on the route:

| Tier | Routes | Refill | Burst |
|---|---|---|---|
| auth | `/auth/basic/login`, `/auth/basic/register`, `/auth/check-username`, `/auth/verify-email`, `/auth/resend-verification`, `/auth/oauth/*`, `/auth/oidc/*` | 1 req / 6 s | 5 |
| webhook | `/hooks/*` | 1 req / s | 30 |
| cache | `/cache/{cache}/*` (public NAR surface) and `/cache/{cache}/proto` | 1 req / 20 ms | 3000 |
| cache-inspect | `/cache/{cache}/{ls,serve}/*` | 1 req / 333 ms | 180 |
| cache-log | `/cache/{cache}/log/*` | 1 req / 333 ms | 900 |
| cache-upload | `POST /caches/{cache}/nars`, `PUT /caches/{cache}/nars/{hash}/chunk`, `POST /caches/{cache}/nars/{hash}/finalize` | 1 req / 2 ms | 10000 |
| default | everything else | 1 req / 200 ms | 150 |

Client IP is taken from `X-Forwarded-For` / `X-Real-IP` (deployments are
expected behind a reverse proxy), falling back to the connection peer IP.

## Request Tracing

Every response carries an `x-request-id` header. If the client (or an
upstream reverse proxy) supplies an `x-request-id` on the request, it is
preserved verbatim; otherwise the server mints a UUID v7. Use this id to
correlate client-side traces with server logs - every log line emitted
while the request is in flight (handler, DB, spawned cleanup tasks)
carries the same id.

## Common Response Envelope

Every JSON response is wrapped in a standard envelope:
```json
{ "error": false, "message": <payload> }
```
On errors `"error"` is `true` and `"message"` contains the error description.

## Nix Binary Cache

The integrated Nix binary cache lives outside `/api/v1`. Configure Nix with:
```
substituters = https://gradient.example.com/cache/mycache
trusted-public-keys = gradient.example.com-mycache:...
```
The public key is returned by `GET /api/v1/caches/{cache}/public-key`.

## State Management

Objects whose `managed` field is `true` are controlled by the NixOS module
configuration and cannot be mutated through the API.