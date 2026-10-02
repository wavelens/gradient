# Authentication

HTTP requests are proving their identity with session JWTs, API keys, download tokens or OIDC sign-in. Worker authentication on `/proto` is covered on [Connection](../proto/connection.md).

```mermaid
flowchart LR
    req[Request] --> src{"Bearer header<br/>or jwt_token cookie"}
    src -->|"GRAD..."| key[API key: SHA-256 lookup]
    src -->|JWT| jwt[Session JWT: HS256]
    jwt --> session[("session row: revoked or expired")]
    key --> checks["expiry, revocation, pin,<br/>permission mask, allowed_ips"]
```

## Tokens

| Token | Format | Lifetime | Minted by |
|---|---|---|---|
| Session JWT | HS256 with `GRADIENT_SECRETS_JWT_FILE`. Claims `{ exp, iat, id, jti }`, with `jti` naming the `session` row | 24 h, 30 days with `remember_me` | `create_session_and_token` |
| API key | 64 random alphanumeric characters, stored as SHA-256 hex, returned with a `GRAD` prefix | `expires_at` or none | API key endpoints |
| Download token | HS256 JWT with `derivation` and `evaluation` claims | 1 h | `encode_download_token` |

- Each request is checking the session row for revocation and expiry.
- An API key is carrying `expires_at`, `revoked_at`, an optional project or cache pin, a permission mask and `allowed_ips`.
- The server is stamping `api.last_used_at` and `session.last_used_at` at most once a minute (`LAST_USED_STAMP_INTERVAL`).
- A failed stamp is only producing a log line, never a fatal error.

## OIDC

```mermaid
sequenceDiagram
    participant B as Browser
    participant G as Gradient
    participant P as Provider
    B->>G: oidc_login_create
    G->>B: redirect + signed oidc_csrf cookie (state, nonce, PKCE verifier, 10 min)
    B->>P: sign in
    P->>B: redirect with code
    B->>G: oidc_login_verify
    G->>P: code + code_verifier
    P->>G: ID token
    G->>G: verify against JWKS, upsert user
    G->>B: session JWT
```

- PKCE is using S256.
- Endpoints come from `<discoveryUrl>/.well-known/openid-configuration`.
- `oidc_login_verify` is returning the user.
- The endpoint is minting the session.

## Related

- [API](../../reference/api.md): the header and API key usage
- [Set Up Single Sign-On](../../guides/sso.md): provider configuration
