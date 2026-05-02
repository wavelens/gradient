# Gradient Backend — Security & Code-Quality Audit

**Scope:** `backend/` workspace (web, core, cache, scheduler, worker, migration, entity, proto).
**Lens:** "How would I build this from scratch today, ignoring legacy?" — findings are not constrained by what's easy to fix.
**Severity:** Critical / High / Medium / Low / Nit.

A finding's severity reflects worst-case impact on a public deployment. Some highs are only relevant when a specific feature flag is on (e.g. OIDC); those are noted.

## Executive summary

The backend is structurally sound for a hobby/internal deployment but has several **publicly-exposed-deployment-blocking** defects, plus a sizeable backlog of code-quality and DRY issues.

### Top security findings

| # | Title | Severity | Section |
|---|------|----------|---------|
| 1 | Any org member can promote themselves to admin, kick founders, delete orgs | Critical | 2.1 |
| 2 | OIDC has no `state`/`nonce`, no ID-token verification, merges by email | Critical (×3) | 1.1 / 1.2 / 1.3 |
| 3 | Authenticated multipart upload writes attacker-controlled paths anywhere on disk | Critical | 5.1 |
| 4 | Outgoing webhooks fetch any URL — full SSRF, cloud-metadata reachable | Critical | 4.1 / 11.4 |
| 5 | SSH-key decryption silently accepts plaintext-base64 stored in the DB | High | 11.5 |
| 6 | `user::delete` unauthenticated by password — stolen JWT permanently destroys an account | High | 2.5 |
| 7 | API keys: no expiry, no revocation, no scopes; equal to a session JWT | High | 1.6 |
| 8 | JWT secret re-read from disk on every request; `process::exit` on read failure | High | 6.1 |
| 9 | Worker-peer token compare is not constant-time; tokens stored as bare SHA-256 | High | 11.1 / 11.2 |
| 10 | `/proto` WebSocket has no Origin check (CSWSH) | High | 11.3 |
| 11 | `write_key` persists decrypted SSH keys to `/tmp` with no RAII cleanup | High | 11.6 |
| 12 | Several state-changing endpoints check membership but not role | High | 11.9 |
| 13 | Lost-update race on cache metrics | High | 7.1 |
| 14 | No body-size limits → trivial OOM via multipart | High | 5.2 / 7.3 |
| 15 | New migration likely won't compile (missing `Write` import) | High / Compile | 5.8 |
| 16 | **Zero rate limiting in the entire HTTP layer** — login/Argon2 DoS, public NAR amplification, free SMTP relay, SSRF amplifier | High (×6) | 18 |

### Top code-quality findings (DRY / structure)

| # | Title | Section |
|---|------|---------|
| A | Three independent role-check helpers; `load_editable_org` is misnamed and bug-prone | 12.1 |
| B | `BaseResponse<T>` envelope wraps every response; 122 manual constructions | 12.2 |
| C | `EvaluationStatus::is_active` is duplicated 7+ times instead of a method | 12.3 |
| D | `state/provisioning.rs` (1205 LOC) is one method-per-resource copy-paste | 12.4 |
| E | `caches/helpers.rs` defines a struct purely to thread `state`, then re-wraps as free fns | 12.6 |
| F | Multipart parsing is hand-rolled instead of `#[derive(TryFromMultipart)]` (also fixes 3 security bugs) | 12.7 |
| G | `secret_file: String` cloned into every helper; `&str` is correct | 12.9 |
| H | `serve_url.replace("https://", "")` dance copy-pasted 4× | 12.14 |
| I | `WebError`'s 14 variants and helper soup should collapse to ~6 with `thiserror` and stable `error_code` | 12.10 / 12.11 |
| J | `pub use self::*::*;` in mod.rs files hides where things come from | 12.8 |

### A reasonable first sprint

1. Fix the org RBAC structurally (§12.1's `OrgRole::require` extractor). This kills §2.1 and §11.9 in one stroke and shrinks every endpoint.
2. Replace OIDC with `openidconnect` crate.
3. Rewrite the multipart handler with `axum-typed-multipart` — fixes §5.1 / §5.2 / §5.3 / §12.7 simultaneously.
4. Add an SSRF guard layer for outbound HTTP (webhooks, OIDC discovery, GitHub manifest). Pin DNS, refuse RFC1918/link-local/loopback, no redirects.
5. Add the rate-limit / body-limit / timeout / concurrency-limit layer described in §18.11, exposed via §18.12's nix module options. This single change shuts down a half-dozen DoS vectors at once.
6. Load secrets once into `ServerState` with `ArcSwap` (§6.1) and delete the SSH plaintext fallback (§11.5).

Everything else can be staged incrementally as files are touched.

---

## 1. Authentication & session

### 1.1 [CRITICAL] OIDC flow has no `state` and no `nonce`
File: `backend/web/src/authorization/oidc.rs:80-91`, `:93-167`

`oidc_login_create` builds the authorization URL with only `response_type`, `client_id`, `redirect_uri`, `scope`. There is no `state` parameter, no nonce, and nothing is persisted server-side. `oidc_login_verify` then accepts an `authorization_code` from `?code=` with no anti-CSRF binding. Consequences:

- **Login CSRF / session fixation**: An attacker initiates the OIDC flow, obtains a code, and tricks a victim into hitting `/api/v1/auth/oidc/callback?code=<attacker-code>`. The victim is now logged into the attacker's account and any data they upload (SSH keys, projects, API keys) flows to the attacker.
- **No replay protection** on the code exchange itself.

Ideal fix: generate `state` (random 128-bit), persist with TTL keyed by user/session, require it on callback. Issue and verify a `nonce` claim against the ID token.

### 1.2 [CRITICAL] OIDC ID-token signature is never verified
File: `backend/web/src/authorization/oidc.rs:121-167`

After exchanging the code for tokens, the server reads only `access_token`, ignores `id_token` entirely, then calls `userinfo_endpoint` and trusts whatever JSON comes back. The ID-token JWT is never decoded or signature-verified against the IdP's JWKS, and the userinfo response itself is also trusted without inspecting `iss`/`aud` (the `OidcUser` struct has those fields but nothing checks them).

Effect: if an attacker can MITM the userinfo endpoint, return an arbitrary `email` from a compromised IdP-adjacent host, or run a second OIDC server that returns chosen `email`/`sub`, they can authenticate as any user (see 1.3).

Ideal fix: verify the ID-token JWT against the IdP's JWKS, validate `iss == discovery.issuer`, `aud == client_id`, `exp/iat`, and `nonce`; treat `sub` (not `email`) as the stable identifier.

### 1.3 [CRITICAL] OIDC merges accounts by email and silently overwrites identity fields
File: `backend/web/src/authorization/oidc.rs:170-260`

`create_or_update_user` does `Email == userinfo.email OR Username == preferred_username`. If a row matches, the handler updates `email`, `username`, and `name` from the OIDC payload without any verification, and only refuses if the existing row has a `password` (basic-auth user).

Two takeover paths:

1. **Email-based takeover**: any IdP that lets a user set arbitrary `email` (most do for non-verified emails, plus self-hosted IdPs configured by attackers if discovery is misconfigured) can claim an existing OIDC-only account by matching its email.
2. **Username squatting → identity churn**: distinct OIDC subjects collapse onto the same row, and `username` keeps flipping. Audit-log forensics break.

Ideal: identify users by `(issuer, sub)` only; store this in a dedicated `oidc_identity` table; never merge by email; require an explicit "link account" flow guarded by the existing session.

### 1.4 [HIGH] JWT decode error returns 500
File: `backend/web/src/authorization/jwt.rs:125-131`

```rust
decode(&jwt, &DecodingKey::from_secret(...), &Validation::default())
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
```

Any malformed/expired/forged token yields HTTP 500. The middleware at `middleware.rs:67-78` then re-maps the err to 401, but inside `decode_jwt` the call site loses semantic info, and any caller that surfaces the raw `StatusCode` (e.g. download tokens elsewhere) reports server errors for client-side problems. Should be `UNAUTHORIZED` everywhere.

### 1.5 [HIGH] JWT validation algorithm is not pinned
File: `backend/web/src/authorization/jwt.rs:128`, `:161`

`Validation::default()` accepts only HS256 in current `jsonwebtoken` defaults, but encoding uses `Header::default()` which is also HS256, and there's no explicit `Validation::new(Algorithm::HS256)` or `validation.set_required_spec_claims(&["exp", "iat"])`. The defensive habit is to pin both. Also `iat` is set but never checked (no `iat_validation`).

### 1.6 [HIGH] API-key path bypasses every claim check
File: `backend/web/src/authorization/jwt.rs:92-121`

When the token starts with `GRAD`, `decode_jwt` constructs a synthetic `Cliams { exp: 0, iat: <created_at>, id: api_key.owned_by }`. There is:

- No expiry check on API keys (they're forever valid).
- No revocation flag (a `revoked_at`/`disabled` column is absent — see entity audit below).
- No scope/permission separation: an API key has the *exact same* power as a logged-in browser session belonging to its owner, including admin ops if the owner is a superuser.

Ideal: API keys should be a separate principal type with scopes (`cache:read`, `cache:write`, `build:trigger`, `admin:*`), an explicit `revoked_at`, and optional `expires_at`.

### 1.7 [HIGH] `extract_bearer_or_cookie` and middleware diverge
File: `backend/web/src/authorization/{jwt.rs:48-61, middleware.rs:31-65}`

`authorize` (mandatory auth) re-implements bearer parsing inline and rejects with 403 on header errors. `authorize_optional` uses the shared `extract_bearer_or_cookie`. Two parsers, two behaviours, easy to drift. Header parse failure should also be 401, not 403 — 403 means "authenticated but not allowed".

### 1.8 [HIGH] No CSRF protection on cookie-authenticated state-changing routes
File: `backend/web/src/authorization/middleware.rs`

`jwt_token` cookie is `SameSite=Strict` (good), but the same handler accepts the same JWT via `Authorization: Bearer`. SameSite alone is not a substitute for CSRF tokens on cross-origin POSTs to a deployment that intentionally exposes its API to JS clients. Combined with `allow_credentials(true)` in CORS (`lib.rs:46-49`), and the explicit allowlisting of `serve_url` and `http://<ip>:8000`, a misconfigured `serve_url` (e.g. `*` or attacker domain) becomes a full account takeover. Recommend rejecting cookie auth on requests that lack a non-CORS-safelisted custom header (`X-Requested-With` etc.) — defence in depth.

### 1.9 [MEDIUM] Logout doesn't invalidate the JWT
File: `backend/web/src/endpoints/auth.rs:287-304`

`post_logout` clears the cookie but the JWT remains cryptographically valid until `exp`. With `remember_me=true` that's 30 days. There is no token revocation list.

Ideal: store JWTs by `jti` in a `revoked_tokens` table or rotate the per-user key (versioned `secret_version` claim).

### 1.10 [MEDIUM] Username enumeration on registration
File: `backend/web/src/endpoints/auth.rs:73-84, 319-349`

`/auth/check-username` and registration both leak whether a username exists. Unauthenticated callers can scrape the user list. `post_basic_login` returns the same `invalid_credentials` for both bad-user and bad-password (good), but the verification-resend endpoint (`post_resend_verification:404`) and registration (`already_exists`) leak.

### 1.11 [MEDIUM] No login-attempt rate limiting / lockout
Search of the codebase shows no per-IP or per-user rate limit on `post_basic_login`, `post_resend_verification`, `post_check_username`, OIDC callback. Combined with bcrypt-style hashing inside `password_auth::verify_password` (which is slow, ~100ms), this is *less* abusable than usual, but enumerating usernames + spraying common passwords is wide open.

### 1.12 [MEDIUM] `Cliams` typo
File: `backend/web/src/authorization/jwt.rs:21`

The struct is named `Cliams` (and re-exported through `mod.rs:11-14`). Public type — needs to be renamed `Claims`. Eight call sites; mechanical.

### 1.13 [MEDIUM] `email_verified: true` is set on OIDC user creation regardless of IdP
File: `backend/web/src/authorization/oidc.rs:243`

Hard-coded comment says "OIDC users are considered verified". This is true *only* if the IdP returns `email_verified: true` in its userinfo and you trust the IdP. Right now any OIDC IdP — including a malicious/misconfigured one — produces a verified email account.

### 1.14 [LOW] `encode_download_token` does not bind to user/IP
File: `backend/web/src/authorization/jwt.rs:136-151`

A 1-hour download token contains only `build_id`. Anyone who obtains the URL during that hour can fetch the artefact. For private builds this is a leak channel. Bind `claim.user_id` and check it against the build's project ACL on download.

---

## 2. Authorization, IDOR, multi-tenancy

### 2.1 [CRITICAL] Org RBAC is structurally absent — any member can elevate, kick, rename, and delete
File: `backend/web/src/endpoints/orgs/{members.rs:111-203, management.rs:360-423}`, `mod.rs:31-55`

`load_org_member`/`load_editable_org` only verify the caller is *some* member of the org. Role is never inspected. Consequences for any read-only org member:

- `post_organization_users` — add anyone with role `"admin"` (line 111-152). The new role is loaded by name from `Role` rows that have `Organization == this_org OR Organization IS NULL`, with no check that the calling user has permission to grant it.
- `patch_organization_users` — promote anyone (including themselves) to `admin`. The role lookup at `members.rs:167-171` doesn't even filter by org — a role row from *any* org with the requested name is accepted.
- `delete_organization_users` — kick any other member, including the org founder.
- `patch_organization` — rename the org (collides with anyone else's slug).
- `delete_organization` — destroy the org, all its projects, evals, builds.
- `post_organization_public` / SSH-key generation / GitHub-App linkage — same gap (presumed; same `load_org_member` pattern, see 2.2).

This is the single largest defect in the codebase. The role column on `organization_user` is essentially decorative. **Recommendation**: thread a `RequireRole(min: Role)` extractor that loads `(org, membership, role)` and refuses below the requested rank; replace every `load_editable_org`/`load_org_member` callsite with it; add a model invariant that prevents removing the last admin.

### 2.2 [HIGH] No protection against last-admin removal / orgs orphaned with no admin
`delete_organization_users` and self-leave (if any) don't enforce "at least one admin remains". An org can be wedged into a state where no caller can administer it. Combined with 2.1 this is trivially weaponised; even with 2.1 fixed, accidental loss is plausible. Add a transactional guard: `count(role=admin) > 1 OR target.role != admin`.

### 2.3 [HIGH] `get_recent_direct_builds` only queries orgs the user *created*
File: `backend/web/src/endpoints/builds/direct.rs:178-181`

Filters by `COrganization::CreatedBy.eq(user.id)`. A regular admin (not the founder) of an org sees no direct builds. This is wrong but fail-closed (it under-shows). Same pattern needs to switch to `EOrganizationUser`.

### 2.4 [HIGH] `user::get_search` returns any user globally with no scope and no rate limit
File: `backend/web/src/endpoints/user.rs:59-84`

Any authenticated user can call `GET /user/search?q=a` and walk the entire user table 10 rows at a time by varying the prefix. There's no auth scoping (e.g. "only users I share an org with"), no rate limit, no minimum query length. For an enterprise install this is full PII disclosure.

### 2.5 [HIGH] `user::delete` takes no password / re-auth — stolen JWT permanently destroys an account
File: `backend/web/src/endpoints/user.rs:105-119`

`DELETE /user` deletes the row outright. The TODO at line 109 admits the cascade isn't audited. A leaked session cookie or one CSRF (1.8) is enough to wipe an account. Re-prompt for password (basic auth) or for a second-factor / fresh-session window of 5 minutes.

### 2.6 [HIGH] Email change has no re-verification
File: `backend/web/src/endpoints/user.rs:276-287`

When `email_require_verification` is on at registration, this is a verification bypass: change the email post-signup and the new address is implicitly trusted. Same TOCTOU on the uniqueness check (SELECT … then UPDATE).

### 2.7 [MEDIUM] All "is name available" / unique-on-write endpoints are SELECT-then-INSERT
Files: `caches/management.rs:164-194`, `orgs/management.rs:242-280`, `auth.rs:73-84`, `user.rs:152-160, 257-264, 277-284`

Two concurrent requests can both pass the existence check and both proceed to insert; the second insert relies on the DB unique constraint to fail, which surfaces as a `WebError::Database("Database error")` (500). Lift unique constraints to first-class errors via `DbErr::RecordNotInserted` matching, and remove the pre-check entirely.

### 2.8 [MEDIUM] Cache / build access uses `CreatedBy.eq(user)` OR org subscription, but never role
File: `backend/web/src/endpoints/caches/helpers.rs:55-83`, `management.rs:65-81, 230-244`

Every member of any org subscribed to a cache can read it (correct), but every member can also be denied editing only by *not being the creator*. There's no concept of "cache admins" within an org — only the human who hit `PUT /caches` can ever rename it, including after they leave the org. Move ownership to the org and gate edits on `org_admin`.

---

## 3. Cache & NAR endpoints (path-traversal / SSRF)

### 3.1 [HIGH] `upstream_nar` concatenates user-controlled path into upstream URL
File: `backend/web/src/endpoints/caches/nar.rs:142-157`

```rust
let nar_url = format!("{}/{}", base_url.trim_end_matches('/'), path);
reqwest::Client::new().get(&nar_url).send().await?
```

`path` comes from the route `/cache/{cache}/nar/upstream/{upstream_id}/{*path}` — the `{*path}` glob accepts arbitrary characters, including `..`, `?`, `#`, `@`, and raw URLs. While `base_url` comes from a trusted `cache_upstream` row, a malicious `path` can:

- Use `?@evil.com/poison` to splice a different host into the URL after a redirect.
- Embed `\r\n` for header injection if reqwest's URL parser allows it (it shouldn't, but the test should be explicit).
- Pivot to a different path on the upstream than what was intended.

Ideal: validate `path` against `^[a-zA-Z0-9._-]+\.nar(\.xz|\.zst|\.bz2)?$`, reject anything else. Even better, parse `base_url` once at the call site, then `url.set_path(&format!("/nar/{path}"))` so reqwest can refuse malformed components.

### 3.2 [MEDIUM] `nar` path validation is loose
File: `backend/web/src/endpoints/caches/nar.rs:24-29`

```rust
if !(path.ends_with(".nar") || path.contains(".nar.")) { 404 }
```

`get_hash_from_url(path)` is the real gate, but the suffix check is sloppy: `xx.nar.thing/etc` passes. Anchor with a strict regex.

### 3.3 [MEDIUM] No `Content-Disposition` / size limits on NAR responses
File: `backend/web/src/endpoints/caches/nar.rs:47-53`

The full NAR is read into memory (`compressed: Vec<u8>`) then sent as `Body::from(...)`. For multi-GB closures this is an OOM vector accessible to any anon caller of a public cache. Stream from `nar_storage` via `Body::from_stream`.

### 3.4 [LOW] Spawned metric task swallows DB errors
File: `backend/web/src/endpoints/caches/nar.rs:118-139`

`let _ = state.db.execute(...).await;` — silent failure. Use `if let Err(e) = ... { tracing::warn!(...) }`.

---

## 4. Webhooks & forge integrations

### 4.1 [CRITICAL] Outgoing webhooks have no URL validation — full SSRF surface
File: `backend/core/src/ci/webhook.rs:48-62, 239-281`

`webhook.url` is taken from the row and passed to `reqwest::Client::post(url)` with no validation. An org admin (or any member, given §2.1) can register a webhook pointed at:

- `http://127.0.0.1:5432/...` (probe internal services).
- `http://169.254.169.254/latest/meta-data/...` (AWS instance metadata, IAM role tokens).
- `http://[::1]:9090/metrics` (internal Prometheus).
- File-style URLs: `reqwest` defaults to disallowing those, but a bad config can accidentally enable it.

The signature is computed *over the body*, not the URL, so signing doesn't help. A tenant builds a payload + signature pair the operator cares about and uses the gradient server as a confused deputy.

Ideal: parse the URL, require `https`, resolve the hostname, refuse RFC1918 / loopback / link-local / cloud metadata IPs, refuse mDNS/internal DNS suffixes, and pin the resolved IP across redirects. (See `hyper-util`'s [`StaticResolver`] pattern or libraries like `safe-redir`.)

### 4.2 [HIGH] No HMAC over webhook timestamp; no replay protection
The signature is `sha256(body)`. Receivers cannot tell when a delivery happened, so a captured request can be replayed against the receiver indefinitely. Add `X-Gradient-Timestamp` and include it in the HMAC; receivers reject ±5 min skew.

### 4.3 [HIGH] Outgoing webhook redirects are followed silently
File: `backend/core/src/ci/webhook.rs:39-44`

`reqwest::Client::builder().timeout(...)` — default redirect policy is followed up to 10 hops. Combined with 4.1, an attacker registers `https://attacker.com/redirect?to=http://169.254.169.254/...`. Add `redirect(Policy::none())` and explicitly accept only 2xx.

### 4.4 [MEDIUM] `verify_forge_signature` GitLab branch leaks length
File: `backend/web/src/endpoints/forge_hooks/mod.rs:208-214`

`token.as_bytes().ct_eq(secret.as_bytes())` — `subtle::ConstantTimeEq` for slices short-circuits on length mismatch. Length leaks are usually harmless for fixed secrets, but the GitLab token here can be of arbitrary length (operator-defined), and the comparison reveals it. Pad / hash both sides first: `ct_eq(sha256(a), sha256(b))`.

### 4.5 [MEDIUM] Forge webhook HMAC uses Gitea/Forgejo signature header without `X-Gitea-Delivery` replay nonce
Same replay class as 4.2. Less critical because forge hooks are inbound (only attacks the gradient server) but still worth a delivery-ID dedupe table with TTL.

### 4.6 [LOW] Forge webhook DB errors are reported as 500 with sanitized message
File: `backend/web/src/endpoints/forge_hooks/mod.rs:131-148`

Already returns `"internal error"` to the caller (good), but the secret-decryption failure path also returns 500. A well-formed but mis-encrypted secret typically means operator misconfiguration — exposing this as `BadRequest` and logging at `error!` would speed debugging.

### 4.7 [HIGH] No size cap on webhook payloads (inbound)
File: `backend/web/src/endpoints/forge_hooks/mod.rs:46-50, 115-120`

`body: Bytes` reads the entire request body into memory. A forge with a leaked HMAC secret (or even an attacker who triggers signature failures cheaply) can submit huge bodies. Apply `axum::extract::DefaultBodyLimit` and `tower::limit::RequestBodyLimitLayer` globally with a per-route override for upload endpoints.

---

## 5. Input validation & ORM / SQL

### 5.1 [CRITICAL] Direct-build multipart upload writes attacker-controlled paths
File: `backend/web/src/endpoints/builds/direct.rs:97-114`

```rust
let file_path = format!("{}/{}", temp_dir, filename);
if let Some(parent) = std::path::Path::new(&file_path).parent() {
    fs::create_dir_all(parent).await?;
}
fs::File::create(&file_path).await?;
```

`filename` is the suffix of the multipart field name `file:<filename>`. An authenticated org-member can post a part named `file:../../../../../etc/cron.d/owned` and write a file there. Two consequences:

1. **Server-side file write to anywhere the process can write** — depending on deployment, a writable `/etc/cron.d`, `/var/lib/systemd`, or the `serve_url` web root yields code execution or privilege escalation.
2. **Symlink/TOCTOU**: if `temp_dir` already contains symlinks, `create_dir_all` and `File::create` happily traverse them.

Fix: reject filenames containing `..`, leading `/`, `\`, NUL; canonicalize the joined path with `std::path::absolute` and assert it `starts_with(&temp_dir)` after canonicalization; consider unpacking via a vetted library (e.g. `tar` with `Entries::set_overwrite(false)` and entry-by-entry path checks).

### 5.2 [HIGH] No body / file-size limit on direct-build upload
File: `backend/web/src/endpoints/builds/direct.rs:38-71`

`field.bytes().await` collects the entire field into memory (`Vec<u8>`). One request with a 10 GB part exhausts RAM. Apply `DefaultBodyLimit` (axum) globally and `field.size_hint()` per-part with a hard cap (`100 MB`?). Stream to disk via `field.chunk()` rather than buffering.

### 5.3 [HIGH] Direct-build temp_dir is never cleaned up on error paths
File: `backend/web/src/endpoints/builds/direct.rs:91-161`

Any DB insert failure between `create_dir_all` (line 92) and the `direct_build.insert` at line 159 leaves the upload on disk forever. Use a RAII `TempDir` (`tempfile::TempDir`) that's `forget()`'d only on success.

### 5.4 [MEDIUM] `post_basic_login` accepts a `loginname` filter that hits `Username = ? OR Email = ?` without input checks
File: `backend/web/src/endpoints/auth.rs:148-156`

Sea-orm parameterises queries safely (no SQL injection), but a 200 KB `loginname` is happily round-tripped to PG. Cap input lengths at the deserializer layer with `#[serde(deserialize_with = "max_len_256")]` or via a newtype.

### 5.5 [MEDIUM] No validation on `derivation` / `wildcard` strings
Files: `direct.rs:23-24, 154`, project / evaluation endpoints

User-provided drv path / wildcard is accepted verbatim and stored. While the Nix evaluator should refuse malformed inputs, defence in depth says reject bytes outside `[A-Za-z0-9._/+-]` early. Crucially, anything starting with `/etc/...` or containing `..` should fail before the worker ever sees it.

### 5.6 [MEDIUM] Raw SQL with PG-only backend in `caches/nar.rs:118-139`
File: `backend/web/src/endpoints/caches/nar.rs:124-138`

`DatabaseBackend::Postgres` is hard-coded. If `state.db` is ever swapped for SQLite (the migration crate can target both), this query silently malfunctions. Use the sea-query DSL or branch on `state.db.get_database_backend()`.

### 5.7 [LOW] `serde_json::Value` indexing with `.as_str().unwrap_or_default()` everywhere in OIDC
File: `backend/web/src/authorization/oidc.rs:157-165`

Silently producing empty strings for missing claims masks bugs. Use `.context("missing claim X")?` and bail early.

### 5.8 [HIGH/COMPILE-FAIL] New migration `m20260502_000000_hash_api_keys.rs` uses `write!` without `use std::fmt::Write`
File: `backend/migration/src/m20260502_000000_hash_api_keys.rs:26`

```rust
write!(&mut out, "{:02x}", b).unwrap();
```

The `write!` macro for a `String` requires the `std::fmt::Write` trait to be in scope. The neighbouring file `web/src/authorization/jwt.rs:180` adds it inside its own `for`-loop scope; this migration does not. Looks like it would fail to compile — needs `use std::fmt::Write;` at the top of the file, OR the simpler `format!("{bytes:02x?}")`/`hex::encode(bytes)` (the project already pulls in `hex`).

(Note: file is currently untracked per `git status`; flag for verification before commit.)

### 5.9 [LOW] Migration `down()` is a no-op
Same file, line 59-61. Acceptable for irreversibly hashing data, but should explicitly bail (`Err(DbErr::Custom("non-reversible".into()))`) so a reckless `migrate down` doesn't silently no-op past it and leave the db in a confusing state.

---

## 6. Secret handling

### 6.1 [HIGH] `load_secret` re-reads the secret file from disk on every authenticated request
File: `backend/core/src/types/input.rs:219-233`, called from 16+ sites including `jwt.rs` (every `decode_jwt`/`encode_jwt`), `webhook.rs` (every delivery), `cache_key.rs` (every cache sign), `oidc.rs` (every OIDC verify).

Every JWT validation does a `fs::read_to_string`. Three problems:

1. **Performance**: tens of thousands of unnecessary syscalls/sec under load.
2. **Correctness during rotation**: replacing the file mid-flight invalidates *every* outstanding JWT instantly with no grace window — operators can't roll secrets safely.
3. **DoS via fs error**: `process::exit(1)` (line 222, 229) terminates the *entire web server* when a single request fails to read the secret. A node with a flaky FS, or a transient EMFILE, takes the server down on the next request.

Ideal: read each secret once at startup, store as `Arc<SecretString>` on `ServerState`. Provide a `SIGHUP`/`/admin/reload` handler that swaps an `ArcSwap<SecretString>` in place, supporting overlapping old/new keys for a configurable grace window (versioned `kid` claim).

### 6.2 [HIGH] `load_secret_bytes` silently base64-decodes "short" plaintexts
File: `backend/core/src/types/input.rs:239-270`

A 12-character secret is treated as base64 and decoded — likely producing garbage that the operator never intended to use as the actual secret. Two encodings, one input field, no `kind` discriminator. Pick one, document it in `docs/`, refuse the other:

```toml
# Recommended: secret_kind = "raw" | "base64"
```

### 6.3 [HIGH] No minimum entropy / strength check on secrets
A 16-character ASCII passphrase passes (`as_bytes.len() >= 16`). HMAC-SHA256 with a 16-byte low-entropy key is brute-forceable offline if an attacker can capture one signed value. Require base64 of ≥32 cryptographically-random bytes (one-line note in install docs and a startup check that bails on obvious low-entropy strings — e.g. detect English words, repetition).

### 6.4 [MEDIUM] `load_secret` strips `\u{0019}` (`char::from(25)`, EM control char)
Same file, line 225. Magic with no comment and no test. Either remove it or document the upstream tool that produces it (looks like a `kubectl exec` quirk?). Don't ship undocumented input mangling in a security-critical path.

### 6.5 [MEDIUM] `crypter` crate (0.3) — review burden
`backend/core/Cargo.toml:48`. `crypter = "0.3"` is a small, less-audited dependency used to seal:

- Cache signing keys (cache_key.rs).
- SSH private keys for org git access (ssh_key.rs).
- Webhook secrets (webhook.rs).
- GitHub App private keys (state/provisioning.rs).

Recommend: pin to a specific reviewed version, document the algorithm choice (it appears to be Argon2id-derived AES-GCM via `argon` feature — verify), or migrate to `age` / `aws-lc-rs` / `ring` AEAD with explicit nonces and KDF parameters under our control.

### 6.6 [LOW] `serve_url` is reformatted into the cache signature key name via `replace(":", "-")`
File: `backend/core/src/sources/cache_key.rs:57-62`

`base_url = url.replace("https://", "").replace("http://", "").replace(":", "-")` — fragile string surgery instead of `url::Url`. A `serve_url = "http://example.com:8080/path"` becomes `example.com-8080/path`, which then forms part of the Nix sig key name `example.com-8080/path-mycache:<pubkey>` — Nix may reject names with `/`. Parse with `url::Url`, take `host_str()` + `port_or_known_default()`, never include `path`.

### 6.7 [LOW] `crypt_secret_file` is passed by value (`String`) into hot helpers
Throughout `core/src/sources/*.rs`. Each call clones the path. Switch to `&str` (or `&Path`) — saves allocations and hints that the path is read-only.

---

## 7. Other risks (concurrency, OOM, observability)

### 7.1 [HIGH] `record_nar_traffic` is a textbook lost-update race
File: `backend/web/src/endpoints/stats.rs:67-99`

Two concurrent NAR fetches into the same minute bucket: both SELECT the row, both compute `bytes_sent + delta`, both UPDATE. Final value is *one* delta, not *two*. Replace with `UPDATE … SET bytes_sent = bytes_sent + $1, nar_count = nar_count + 1 WHERE …` (raw SQL or sea-orm `Expr::col(...).add(...)`). For the no-row branch, use `INSERT … ON CONFLICT DO UPDATE`.

### 7.2 [HIGH] Fire-and-forget `tokio::spawn` everywhere swallows errors
Examples: `nar.rs:113-115, 119-139`, `caches/management.rs:347-350`, others. None of these spawned futures push errors anywhere a human can see them. Two issues:

1. A failure in NAR-cleanup leaks bytes on disk and no alert fires.
2. A panic in the spawned task is logged by the tokio default panic handler but doesn't return any useful context (no request_id, no cache_id).

Wrap spawns in a small helper:
```rust
fn spawn_named(name: &'static str, fut: impl Future<Output = Result<()>> + Send + 'static) {
    tokio::spawn(async move {
        if let Err(e) = fut.await {
            tracing::error!(task = name, error = %e, "background task failed");
        }
    });
}
```

### 7.3 [HIGH] No global request-body size limit
Axum applies a 2 MiB default to JSON, but `Bytes` extractors (used by webhooks) and `Multipart` (used by direct builds) bypass it. Add `axum::extract::DefaultBodyLimit::max(N)` at the router root and per-route overrides where genuinely needed.

### 7.4 [MEDIUM] `am.bytes_sent.unwrap()` / `am.nar_count.unwrap()` panic-shaped code
File: `backend/web/src/endpoints/stats.rs:82-83`

Currently safe (the `into_active_model()` produces `Unchanged` values that always unwrap), but `unwrap()` in production code paths is a footgun if the model evolves. Prefer `.try_unwrap().unwrap_or(0)` or the atomic-update fix in 7.1.

### 7.5 [MEDIUM] No rate limiting anywhere — login, password reset, NAR fetch, search
There is no `tower-governor` or equivalent layer. Public NAR endpoints are fully unauthenticated for public caches; an attacker can serve as a free CDN amplifier or simply exhaust DB connections.

### 7.6 [MEDIUM] `lib.rs:32-49` builds a CORS allowlist that hard-codes the debug origin
```rust
let debug_url: http::HeaderValue = format!("http://{}:8000", state.cli.ip.clone()).try_into()?;
```

In production where `ip = 0.0.0.0`, this generates `http://0.0.0.0:8000` — never a real origin, so harmless. But the *intent* is to allow a dev tool, and shipping that allowlist entry into prod risks a future operator setting `cli.ip = <public IP>` and accidentally allowing cross-origin requests with credentials from a colocated server. Gate the debug origin behind `#[cfg(debug_assertions)]` or an explicit `--dev` flag.

### 7.7 [LOW] `expect()` in initialization paths can panic at runtime
File: `lib.rs:39-42`

```rust
state.cli.serve_url.clone().try_into().expect("invalid serve_url")
```

`serve_url` is a CLI-supplied env var. A typo or templating bug crashes the server with a stack trace. Validate at `init_state` time, return `Err`, and exit cleanly with a structured message.

### 7.8 [LOW] `unwrap()` in `middleware.rs:54`
```rust
token.unwrap().to_string()
```
Reachable only when `token.is_none() == false` two lines above, so safe; but the pattern reads as defensive code with a clear bug. Use `if let Some(t) = token { ... }`.

---

## 8. Code quality / structure

### 8.1 [NIT] Public type misnamed `Cliams`
See 1.12.

### 8.2 [NIT] `decode_jwt` tail-position type mismatch hack
File: `backend/web/src/authorization/jwt.rs:114-121`

The synthetic `TokenData { claims: Cliams { exp: 0, ... } }` only exists because the function's return type is `TokenData<Cliams>`. The cleaner shape is to return an internal `Principal` enum with `Session { user_id, exp }` and `ApiKey { user_id, key_id, scopes }` so callers can dispatch on it. Right now everything pretends to be a session JWT and the ApiKey path constructs a fake one.

### 8.3 [NIT] HTTP Basic Auth with username ignored, password as JWT/API key
File: `backend/web/src/endpoints/caches/helpers.rs:36-53`

Nix needs Basic auth, fine. But:

- Username is silently ignored — accept *only* a fixed sentinel (`"gradient-key"`) so misconfiguration fails loudly.
- The path accepts both session JWTs and API keys. Restrict to API keys (so a stolen *session cookie* can't be replayed against `nix copy`).

### 8.4 [NIT] Redundant unique-name endpoint pairs
`get_org_name_available`, `get_cache_name_available`, `post_check_username` all do "is this name free?" with no rate limit. They're convenience for client-side validation but they double the surface and are useful for enumeration (1.10, 2.4). Replace with returning a structured error from the actual create call (`error_code: "name_taken"`).

### 8.5 [NIT] Mega-files: `provisioning.rs` (1205 LOC), `handler_tests.rs` (2446 LOC), `nar_import.rs` (1104 LOC)
Split by concern. State provisioning at 1.2k LOC is a flag that the logic should be extracted into a typed pipeline (parse → validate → reconcile → persist) with a stage per file.

### 8.6 [NIT] Mass `pub use` re-exports in `endpoints/*/mod.rs`
Pattern `pub use self::keys::*; pub use self::management::*;` (e.g. `caches/mod.rs:14-19`) drags every public item into the parent namespace. Hard to tell at the call site which file an identifier comes from. Keep submodule scoping (`caches::keys::get_cache_key`).

### 8.7 [NIT] `error.rs` has 14 variants and a parallel set of constructor helpers
File: `backend/web/src/error.rs`

Tighten:
- Collapse `BadRequest` / `Validation` / `InputValidation` — they all map to 400.
- Collapse `Unauthorized` / `Authentication` — both 401.
- Use `thiserror` consistently rather than a hand-rolled `Display`.
- Store `error_code: &'static str` on each variant so clients can switch on a stable identifier (`"invalid_credentials"` etc.) instead of pattern-matching English strings.

### 8.8 [NIT] Two parsers for `Authorization: Bearer …`
`jwt::extract_bearer_or_cookie` and the inline version in `middleware::authorize`. Pick one, delete the other (see 1.7).

### 8.9 [NIT] `BaseResponse<T>` always wraps even for success-only payloads
The shape `{ "error": false, "message": <T> }` mixes "envelope" and "payload" awkwardly. For 200 responses just return `T`; reserve the envelope for errors. Or, since errors already use the `WebError::IntoResponse` path with the same envelope, drop the field and use HTTP status as the only success/failure signal.

### 8.10 [NIT] `endpoint.rs` deleted but not removed cleanly
`git status` shows `D backend/web/src/endpoint.rs` — file deletion staged but module references audited above show callers still expect the layout. Confirm `mod endpoint;` was removed from `lib.rs` (it was — checked) and that no stale tests reference it.

### 8.11 [NIT] Dual `state.db` and `state.web_db`
Most handlers use `state.web_db`; some (e.g. `nar.rs:124`) use `state.db`. Two pools? Two databases? The reason isn't documented at the type. Either explain at `ServerState` definition or unify.

---

## 9. Schema / data model recommendations

### 9.1 API keys table needs `revoked_at`, `expires_at`, `scopes`, `last_used_ip`
The current `api` table only has `(id, owned_by, name, key, last_used_at, created_at, managed)`. Mature shape:

```text
api_key (
    id           uuid pk,
    owned_by     uuid fk,
    name         text,
    key_prefix   text,         -- first 8 chars of GRAD<raw>, shown to user
    key_hash     bytea,         -- sha256(raw)
    scopes       text[],        -- ['cache:read','build:trigger']
    revoked_at   timestamptz nullable,
    expires_at   timestamptz nullable,
    last_used_at timestamptz,
    last_used_ip inet,
    created_at   timestamptz,
    managed      bool
)
```

### 9.2 OIDC identities table
Rather than one `users` row per email, model an `oidc_identity (issuer, sub) → user` so a single user can link multiple IdPs and email is *not* the merge key.

### 9.3 Audit log
No `audit_log` table exists. With a single org admin able to delete the org, there is no record of who did what. Even a cheap "actor / action / target / timestamp" table is a huge defensive win. Write to it from the same transaction as the action.

### 9.4 Webhook deliveries table
`webhook` rows have URL/secret but no delivery history. Add `webhook_delivery (webhook_id, event_id, status_code, attempted_at, response_body_snippet, signature)` so operators can investigate failures and replay, and so retries can be tracked properly.

---

## 10. Top-line remediation order

1. **Org RBAC**: §2.1 / 2.2 / 2.3 — single largest blast radius. Fix immediately.
2. **OIDC**: §1.1 / 1.2 / 1.3 — full account takeover under common configurations. Fix before next release.
3. **Direct-build path traversal**: §5.1 — authenticated RCE-equivalent on the server. Fix immediately.
4. **Outgoing webhook SSRF**: §4.1 / 4.3 — cloud metadata exposure.
5. **Body / file size limits**: §5.2 / 7.3 — easy DoS.
6. **API key model**: §1.6 / 9.1 — needed before any "API key" feature is taken seriously.
7. **Secret-file read-on-every-call**: §6.1 — performance and FS-DoS in the same change.
8. **Lost-update on metrics**: §7.1 — silent data loss under any real traffic.
9. **JWT decode → 401, not 500**: §1.4 — small but visible.
10. Naming, pruning, schema additions: §8 / §9.

---

## 11. Round-2 security findings (proto / worker / scheduler / state / SSH)

### 11.1 [HIGH] `proto::handler::auth::validate_tokens` does not use constant-time comparison
File: `backend/proto/src/handler/auth.rs:200-224`

```rust
let digest = hex::encode(Sha256::digest(token.as_bytes()));
if digest == *token_hash { ... }
```

A regular `String == String` short-circuits on the first differing byte. The leak is bounded (64 hex chars), but for a worker handshake — which is potentially repeatable from anywhere on the network if `--proto-public` is set — it should use `subtle::ConstantTimeEq`. Even better, drop the hex round-trip and compare raw byte slices.

### 11.2 [HIGH] Worker tokens stored as bare SHA-256 (no salt, no KDF)
Same file. The DB column is `sha256(token)`. Workers' shared-secret tokens are presumably long random strings, but the design choice should be `argon2(token, per-row-salt)` or `hmac(server_key, token)` — defence-in-depth against a future leak-and-grind.

### 11.3 [HIGH] `/proto` WebSocket has no Origin header check
File: `backend/proto/src/handler/mod.rs:32-40`

`ws_upgrade` accepts any upgrade request. While the handshake inside the protocol authenticates workers, this leaves the upgrade itself open to *cross-site WebSocket hijacking* (CSWSH): a logged-in browser can be tricked by an attacker site into opening a `/proto` connection. The protocol won't authenticate (no token) but a malicious server can still extract any pre-auth banners, exhaust connection slots, or interfere with worker capacity counting. Add an `Origin` allowlist on upgrade that mirrors the CORS allowlist; reject browser-originating WS connections.

### 11.4 [HIGH] `webhooks::put` accepts the URL with only an `is_empty()` check
File: `backend/web/src/endpoints/webhooks.rs:122-172`

Confirms §4.1 from the receiver side too: there is *no* `Url::parse(&body.url)`, no scheme allowlist, no host allowlist. This is the *entry point* the SSRF travels through.

### 11.5 [HIGH] `decrypt_ssh_private_key` falls back to "plaintext base64" if decryption fails
File: `backend/core/src/sources/ssh_key.rs:113-142`

If `crypter::decrypt_with_password` returns `None`, the code interprets the stored ciphertext as base64-encoded PEM and accepts it if it starts with `-----BEGIN`. The intent is a one-time legacy migration shim, but it permanently weakens the encryption guarantee: anyone who can write to the `organization.private_key` column (DB compromise, SQL injection elsewhere, a rogue replication target) bypasses encryption entirely. Delete the fallback; provide a one-shot migration that re-encrypts old rows or fails noisily.

### 11.6 [HIGH] `write_key` persists decrypted SSH private keys to `/tmp` with no RAII cleanup
File: `backend/core/src/sources/ssh_key.rs:21-40`

`NamedTempFile::with_suffix(".key")` then `.keep()` strips the auto-delete guard. Cleanup is the caller's job (`clear_key`). Any panic, task cancellation, or early return between `keep()` and `clear_key()` leaks the private key. A green-field design uses RAII:

```rust
struct EphemeralKeyFile { path: tempfile::TempPath }
impl Drop for EphemeralKeyFile { /* TempPath::Drop already removes */ }
```

`TempPath` (not `TempPath::keep()`) auto-removes on drop and gives the same `path()` access.

### 11.7 [HIGH] `core/state/provisioning.rs` reads SSH/cache private keys via `fs::read_to_string` from systemd-credentials path WITH NO PERMISSION CHECK
File: `backend/core/src/state/provisioning.rs:188-206, 358-369`

```rust
let private_key_path = format!("{}/gradient_org_{}_private_key", credentials_dir, state_org.name);
let private_key = fs::read_to_string(&private_key_path)?;
```

`state_org.name` is operator-controlled, but the format string allows path-traversal injection if the name contains `/` — the validators in `input.rs` (`check_index_name`) reject `/`, but the provisioning loader uses the raw name from the state file with no defensive parse. Belt-and-braces: validate, OR canonicalize the joined path and assert it lives under `credentials_dir`.

### 11.8 [MEDIUM] `webhooks::post_webhook_test` swallows JSON serialization failure with `.unwrap_or_default()`
File: `backend/web/src/endpoints/webhooks.rs:266`

```rust
let body_str = serde_json::to_string(&payload).unwrap_or_default();
```

If serialization fails (it can't in this code, but the pattern recurs), an empty body is signed and delivered — silent corruption. Use `?` and propagate.

### 11.9 [MEDIUM] `post_project_active` / `delete_project_active` / `post_project_check_repository` use `load_project` (membership only)
File: `backend/web/src/endpoints/projects/management.rs:413-472`

A read-only viewer can:
- Toggle project active/inactive (mutates state, alters CI behaviour).
- Trigger `post_project_check_repository` which performs an outbound git network request — DoS amplification or scanning vector.

Both should use `load_editable_project` (already exists in the file) — these handlers are an oversight, not a missing feature.

### 11.10 [MEDIUM] `state_org.github_installation_id` overwrite path can let a managed-org config wipe a runtime install
File: `backend/core/src/state/provisioning.rs:232-234`

If state declares `github_installation_id = Some(...)`, it overwrites the existing value. If a re-application happens after a webhook sets the field, an old/stale state file can clobber the live install. The comment correctly notes this is the intended override; what's missing is a hard error when the state value differs from the live one and a flag like `--allow-install-overwrite`.

### 11.11 [LOW] `outbound::connect_to_registered_workers` fetches *all* registrations every 15s
File: `backend/proto/src/outbound.rs:48-58`

`EWorkerRegistration::find().filter(Url.is_not_null()).all(&state.db).await` runs every 15 seconds with no LIMIT. At ~10k registrations this is a steady tail-load on PG. Bound the query and add an index on `(active=true AND url IS NOT NULL)`.

### 11.12 [LOW] No application-level rate-limit on `/proto` upgrade or on outbound connect attempts
Concurrent slot-claim bug in `connecting: HashSet<String>` is fine (mutex-guarded), but a loop of register-and-disconnect can exhaust file descriptors.

---

## 12. Code quality — DRY, structure, type-driven design

These findings are about how the code is *organised*, not what it does. They are the changes I'd make if I were rewriting the project today.

### 12.1 [HIGH/QUALITY] Three role-check helpers exist; `load_org_member` / `load_editable_org` are anti-helpers
Files: `endpoints/orgs/mod.rs:31-55`, `endpoints/projects/mod.rs:157-176` (`user_can_edit`), `endpoints/orgs/settings.rs:35-63` (`require_write_permission`), `endpoints/error.rs:197-203` (`require_superuser`).

Three independently-implemented "does this user have the right role?" functions, each with subtly different signatures and error messages. A fourth (`load_editable_org`) is misnamed — it implies an edit check but only filters out state-managed orgs.

What the codebase wants is one extractor:

```rust
// in authorization::guard
pub struct OrgRole {
    pub org: MOrganization,
    pub user: MUser,
    pub role: Role,             // enum { Admin, Write, Read }
}

impl OrgRole {
    pub async fn require(state: &ServerState, user: &MUser, org_name: &str, min: Role)
        -> WebResult<Self> { ... }
}
```

…and every handler that currently does `load_editable_org` / `load_org_member` / `user_can_edit` becomes:

```rust
let ctx = OrgRole::require(&state, &user, &organization, Role::Admin).await?;
```

This:
- Collapses three helpers into one.
- Encodes the minimum role at the type boundary (impossible to forget).
- Eliminates the §2.1 RBAC bug class entirely — there's no `load_org_member`-shaped function left to misuse.
- Lets the test suite exhaustively check "every state-changing handler requires ≥ Write" by counting call sites.

### 12.2 [HIGH/QUALITY] `BaseResponse<T>` is an envelope nobody asked for
122 occurrences in `endpoints/`. The shape is:

```rust
struct BaseResponse<T> { error: bool, message: T }
```

It's used for *everything*, including success responses where `error: false` is redundant with the HTTP 2xx status. For errors, `error.rs::IntoResponse` already builds an envelope. The result is double-wrapping: the success path builds it manually (`Json(BaseResponse { error: false, message: ... })`), the error path re-builds it via `IntoResponse`. Just return `Json(T)` on success and rely on HTTP status codes; reserve the envelope for errors with a structured `error_code` field.

This is also a massive copy-paste reduction:

```rust
Ok(Json(BaseResponse { error: false, message: organization }))
```

becomes

```rust
Ok(Json(organization))
```

and the type `BaseResponse<T>` collapses to one error type used only inside `WebError::IntoResponse`.

### 12.3 [HIGH/QUALITY] `EvaluationStatus::is_active` doesn't exist — duplicated 7+ times
Search results: 47 references to specific `EvaluationStatus::*` variants in non-test code, with the *exact same six-arm OR pattern* (`Queued | Fetching | EvaluatingFlake | EvaluatingDerivation | Building | Waiting`) appearing in `orgs/management.rs:128-138`, `scheduler/eval.rs:*`, `core/sources/git.rs:115-122`, and elsewhere.

```rust
// entity/src/evaluation.rs
impl EvaluationStatus {
    pub fn is_active(self) -> bool {
        matches!(self,
            Self::Queued | Self::Fetching | Self::EvaluatingFlake
            | Self::EvaluatingDerivation | Self::Building | Self::Waiting)
    }
    pub fn is_terminal(self) -> bool { !self.is_active() }
}
```

Add a single sea-orm filter helper too: `CEvaluation::Status.is_active()` via an extension trait.

### 12.4 [HIGH/QUALITY] `state/provisioning.rs` (~1205 LOC) is one method-per-resource copy-paste
File: `backend/core/src/state/provisioning.rs`

Every `apply_*` method follows the same shape:

1. Resolve `created_by` from a HashMap.
2. Resolve org from a HashMap.
3. Read a credential file from `GRADIENT_CREDENTIALS_DIR`.
4. Encrypt with `crypter::encrypt_with_password(secret, ...)`.
5. `find().filter(name.eq(...)).one(...)` — does it exist?
6. If yes, build an `ActiveModel` from `existing.into()`, set fields, `update`.
7. If no, build a fresh `ActiveModel`, set fields, `insert`.

This calls for an `Upsert` trait and a single generic method:

```rust
trait Upsertable {
    type Source;
    type Active: ActiveModelTrait;
    type Filter;

    fn lookup_filter(src: &Self::Source) -> Self::Filter;
    fn fill_active(am: &mut Self::Active, src: &Self::Source);
    fn build_new(src: &Self::Source) -> Self::Active;
}

async fn upsert<U: Upsertable>(db: &DbConn, src: &U::Source) -> Result<Uuid> { ... }
```

Each `apply_*` then becomes ~30 LOC — `1205 → ~400 LOC` net.

### 12.5 [HIGH/QUALITY] 9× duplicated "is user an org member?" SQL
9 hits of `EOrganizationUser::find().filter(Org.eq(_)).filter(User.eq(_))` — `core/db.rs` already exposes `get_organization_by_name(state, user_id, name)` which does this implicitly, but every other code path re-implements it. Add a single `OrgMembership::lookup(state, user_id, org_id) -> Option<MOrganizationUser>` with role embedded, and use that everywhere (subsumed by 12.1).

### 12.6 [HIGH/QUALITY] `endpoints/caches/helpers.rs` defines `CacheOpsHandler` as a `(state)` struct then exposes free-function wrappers
File: `backend/web/src/endpoints/caches/helpers.rs:25-87, 372-386`

The struct exists *just* to thread `state`; then the file exports `pub(super) fn get_nar_by_hash(state, ...)` wrappers that immediately do `CacheOpsHandler::new(&state).get_nar_by_hash(...)`. This is the ceremony of OO without the value. Either:

- **Delete the struct** and pass `state: &Arc<ServerState>` directly (it's already `Arc`'d, the cost is zero). OR
- **Delete the wrappers** and let callers do `CacheOpsHandler::new(&state).get_nar_by_hash(...)`.

Pick one. The current shape costs LOC and reading time without paying for itself.

### 12.7 [HIGH/QUALITY] Multipart parser in `direct.rs` should be a `MultipartForm` struct + derive
File: `backend/web/src/endpoints/builds/direct.rs:38-71`

The hand-rolled parser:

```rust
while let Some(field) = multipart.next_field().await? {
    let name = field.name().unwrap_or("").to_string();
    if name == "organization" { organization = Some(field.text().await?); }
    else if name == "derivation" { derivation = Some(field.text().await?); }
    else if name.starts_with("file:") { ... }
}
let organization = organization.ok_or_else(|| WebError::BadRequest("Missing organization parameter"))?;
let derivation = derivation.ok_or_else(|| WebError::BadRequest("Missing derivation parameter"))?;
```

is what `axum-typed-multipart`, `axum::extract::Multipart` + a deserialize, or a hand-built `TryFrom<Multipart>` exists for. A struct with `#[derive(TryFromMultipart)]`:

```rust
#[derive(TryFromMultipart)]
struct DirectBuildForm {
    organization: String,
    derivation: String,
    #[form_data(field_name = "file:*")]
    files: HashMap<String, FieldData<NamedTempFile>>,
}
```

…also enforces the file-write-to-disk via `NamedTempFile`, which is RAII (fixes 5.3) and uses framework-supplied size limits (fixes 5.2). Three bugs, one structural change.

### 12.8 [HIGH/QUALITY] `pub use self::*::*;` flattening hides where things come from
Files: `endpoints/caches/mod.rs:14-19`, `endpoints/projects/mod.rs:12-15`, `endpoints/orgs/mod.rs:14-19`, `endpoints/builds/mod.rs:13-17`, `endpoints/evals/mod.rs`, etc.

The pattern `pub use self::keys::*; pub use self::management::*;` lets call sites write `caches::get_cache_key` instead of `caches::keys::get_cache_key`, but it costs:

- Grep for `get_cache_key` returns the call sites with no module path.
- Adding a function to `keys.rs` silently exposes it as a top-level `caches::` item.
- IDE goto-definition jumps through one extra hop.

Drop the re-exports. The route table in `lib.rs` already uses fully-qualified paths.

### 12.9 [HIGH/QUALITY] `secret_file: String` passed by value, cloned per call
Files: `core/src/sources/cache_key.rs`, `core/src/sources/ssh_key.rs`, `core/src/ci/webhook.rs`.

```rust
pub fn generate_signing_key(secret_file: String) -> ... {
    let secret = load_secret_bytes(&secret_file);
    ...
}
```

Should be `&str` (or `&Path`). `String` semantically signals ownership transfer that's never used; every caller does `.clone()`. ~20 sites.

### 12.10 [MEDIUM/QUALITY] WebError should be `thiserror`-derived with a stable `error_code`
File: `backend/web/src/error.rs`

The hand-written `Display`, `From`, and `IntoResponse` impls are all derivable. Plus, every variant ought to carry a stable string identifier so frontend / clients can switch on it:

```rust
#[derive(thiserror::Error)]
pub enum WebError {
    #[error("invalid credentials")]
    #[code("auth.invalid_credentials")]
    InvalidCredentials,
    ...
}
```

This eliminates English-string pattern-matching by clients — see frontend code for `if msg.contains("Invalid credentials")` patterns that rot the second the message changes.

### 12.11 [MEDIUM/QUALITY] 14 variants, 12 helper constructors, 9 collapsible cases
Same file. `BadRequest` / `Validation` / `InputValidation` all map to 400. `Unauthorized` / `Authentication` both map to 401. `Internal` / `InternalServerError` / `Database` / `JsonParsing` all converge on 500. Collapse to 6 status-mapped variants with a typed `kind` for clients.

### 12.12 [MEDIUM/QUALITY] Mass clone of `state.cli.serve_url`, `state.cli.crypt_secret_file`
Many handlers `state.cli.serve_url.clone()` then pass by value. `serve_url` and the secret file path are immutable for the process lifetime; they should be `Arc<str>` on `ServerState`. Same for `crypt_secret_file`, `jwt_secret_file`, `base_path`.

### 12.13 [MEDIUM/QUALITY] `orgs/management.rs::OrganizationSummary` and `OrgResponse` are 90% the same shape
File: `endpoints/orgs/management.rs:43-77`. The list endpoint returns one shape, the detail endpoint returns another. Merge into one `OrganizationView` with optional fields, or use a thin `OrganizationSummary` for the list and `OrganizationView { summary: OrganizationSummary, github_installation_id, ..., role, .. }` for detail. Right now both are kept manually in sync.

### 12.14 [MEDIUM/QUALITY] `format_cache_public_key` and `format_public_key` (orgs) and `format_cache_key` all do the same `serve_url.replace("https://", "").replace("http://", "")` dance
Files: `core/src/sources/cache_key.rs:57-62`, `core/src/sources/ssh_key.rs:150-160`, plus `endpoints/caches/helpers.rs:192-200`.

Same fragile string surgery in 4 places. One helper:

```rust
fn cache_key_hostname(serve_url: &str) -> Cow<str> {
    Url::parse(serve_url).ok()
        .and_then(|u| u.host_str().map(|h| match u.port() {
            Some(p) => format!("{h}-{p}").into(),
            None => h.to_owned().into(),
        }))
        .unwrap_or_else(|| serve_url.into())
}
```

Use everywhere; delete the rest.

### 12.15 [MEDIUM/QUALITY] `*Available` endpoints duplicate code with `put` / `post_basic_register`
`get_org_name_available` / `get_cache_name_available` / `post_check_username` / `get_project_name_available` are four near-identical handlers. Even with their existence justified, factor:

```rust
async fn name_available<E>(db: &DbConn, name: &str) -> WebResult<bool>
where E: EntityWithName, ...
```

…then each handler is a 3-liner.

### 12.16 [MEDIUM/QUALITY] `Cliams` vs `Claims` — one of many spelling/naming issues
- `Cliams` (jwt.rs) → `Claims`.
- `cleanup_nars_for_orgs` (helpers.rs) — orgs is the input, but the function deletes NARs whose *derivations* belong to those orgs *if* no other org subscribes to the cache. Function name is misleading. Better: `gc_nars_orphaned_by_unsubscribe(orgs)` or invert the data flow.
- `wildcard` field on the evaluation row (used in `direct.rs:136`) actually stores the *derivation path* for direct builds; the same column is the wildcard string for project-driven builds. Two semantics, one column. Rename to `eval_target` and use a sum type.
- `serve_url` is sometimes a URL with scheme, sometimes a host; the four `replace("https://", "")` sites are evidence of the confusion.

### 12.17 [MEDIUM/QUALITY] 11 `"Failed to ..."` `.to_string()` strings live in handler code
Move to `WebError` constructors (`WebError::failed_to_X()`). The reason is not just DRY — it's that the strings are part of the API contract for clients that surface them, and they must change in lockstep across the codebase.

### 12.18 [MEDIUM/QUALITY] `record_nar_traffic` should be a typed `MetricsRecorder` background task, not a fire-and-forget spawn
File: `endpoints/stats.rs:67-99`, called from `nar.rs:113-115` via `spawn_nar_traffic_metric`.

A bounded `mpsc::Sender<MetricEvent>` decouples the handler from the DB, batches updates over a 1-second window, applies the atomic UPDATE pattern (§7.1), and lets the receiver flush on shutdown for accurate counters. Fixes the lost-update race and the unbounded-spawn footgun simultaneously.

### 12.19 [LOW/QUALITY] Shared `ServerState` mixes "long-lived config" and "per-request DB"
`ServerState` carries `cli: Cli`, `db: DatabaseConnection`, `web_db: DatabaseConnection`, plus `webhooks`, `email`, `manifest_state`, `pending_credentials`. Two concerns. Split:

```rust
pub struct AppConfig { /* cli, parsed_secrets, urls */ }
pub struct AppDb    { db, web_db }
pub struct AppDeps  { webhooks, email, manifest_state }
pub struct ServerState {
    pub config: Arc<AppConfig>,
    pub db: Arc<AppDb>,
    pub deps: Arc<AppDeps>,
    pub scheduler: Weak<Scheduler>,  // optional – currently in Extension
}
```

Tests can swap `AppDeps` cheaply; production keeps everything `Arc<...>`.

### 12.20 [LOW/QUALITY] `state.db` vs `state.web_db` — undocumented split
Most handlers use `web_db`; the NAR resolver in `caches/nar.rs:124` uses `db`. Why two pools? Read-replica? Migrator vs runtime? The type doesn't say. Either rename to `state.read_db` / `state.write_db`, or unify if there's no reason.

### 12.21 [LOW/QUALITY] Duplicated `MockDatabase` setup in tests
File: `proto/src/handler/auth.rs:455-561` builds the same MockDatabase scaffolding from scratch in every test. A `mock_state(builder: impl FnOnce(MockDatabase) -> MockDatabase)` helper drops 30+ LOC across the test module.

### 12.22 [LOW/QUALITY] `HashMap<String, ...>` sprinkled where `BTreeMap` or a typed key would be clearer
e.g. `endpoints/auth.rs:188`, `oidc.rs:165`. For OIDC userinfo, define a typed `UserInfoClaims` struct and `serde_json::from_value` into it; ditto for query params, where `axum::extract::Query<TypedParams>` is cheaper and self-validating.

### 12.23 [LOW/QUALITY] `StringListItem` is `{ id: String, name: String }`
File: `endpoints/orgs/members.rs:23-27`. A pair-tuple fighting to be a struct, used to mean "(username, role-name)" — but the field names lie because `id` is actually a username. Make it a real type with real names, or use `(String, String)`.

### 12.24 [LOW/QUALITY] Macro / helper soup in tests — `headers_with(name, value)`
File: `authorization/jwt.rs:191-195`. Fine as-is, but pattern recurs in `auth.rs::tests`, `forge_hooks::verify_tests`, etc. with subtle differences. Lift to `test-support` once and import everywhere.

### 12.25 [LOW/QUALITY] `Cargo.toml` per-crate has feature flags out of order
`web/Cargo.toml` lists workspace deps then crate-specific deps with manual alignment. Run `cargo sort --workspace` once and bake into pre-commit.

### 12.26 [LOW/QUALITY] `lib.rs::create_router` is a 200+-line builder
Split by domain: `routes::auth_routes() -> Router<S>`, `routes::cache_routes() -> Router<S>`, etc. Each domain owns its own `mod.rs` plus a single `routes()` function. Keeps adding endpoints from being a `lib.rs` merge-conflict magnet.

### 12.27 [NIT] Inline docstrings vs. comments — `///` used for things that aren't public API
e.g. internal helpers in `endpoints/caches/helpers.rs:25-30`. Use `//` for non-public docs; `///` should map 1:1 with rustdoc HTML.

### 12.28 [NIT] `chrono::Utc::now().naive_utc()` repeated 30+ times
Wrap once: `fn now() -> NaiveDateTime { Utc::now().naive_utc() }`. Tiny, but lets you swap in a clock for tests in one place.

### 12.29 [NIT] `Path<(String, ...)>` extractors should be typed newtypes
`Path<(String, Uuid)>` for `(organization_name, webhook_id)` is six characters of type info short of clarity. Define `OrgName(String)` newtype with a `FromStr` that runs `validate_organization_name`; then it is impossible to forget validation at the handler boundary.

### 12.30 [NIT] `SourceError` variants like `InvalidSshKey`, `KeyDecryption`, `KeyUtf8Conversion`, `KeyPairConversion` could be flattened
File: `core/src/sources/mod.rs` (or wherever defined). Eight variants for "something went wrong with a key" — collapse to `KeyError(KeyErrorKind)` with one inner enum, and the sites still produce useful `Display` strings.

---

## 13. Testing & observability

### 13.1 [MEDIUM] `handler_tests.rs` is 2446 LOC — single file
File: `backend/scheduler/src/handler_tests.rs`. Should be broken up by concern. A 2.4k LOC test file is a sign that test scaffolding is being cut-pasted; each concern probably wants ≤300 LOC and one matching focus.

### 13.2 [MEDIUM] No log redaction layer
Search for `tracing` calls reveals raw `error = %e` for DB errors — these can include parameter values. If a webhook secret or JWT enters a `DbErr` error path, it'll be tracing-logged. Add a `tower::Layer` that scrubs known secret patterns from log output (`Bearer …`, `GRAD…`, `-----BEGIN …`).

### 13.3 [MEDIUM] No request ID / correlation ID
The `TraceLayer` logs request paths but nothing connects a request to its downstream DB log entries or to a webhook delivery. Add `tower-http::request_id` + a `tracing::Span` extension; propagate via the proto WebSocket too.

### 13.4 [MEDIUM] `proto::handler::auth` lookups return `vec![]` on DB error → fail-closed (good) but log only at `warn!`
A persistent DB outage in this hot path is silently degraded auth. Add a metric + `error!` for repeat failures, and surface health to `/health`.

### 13.5 [LOW] `/health` returns `200 ALIVE` unconditionally
File: `endpoints/mod.rs:98-105`. Not a real health check. Should at least probe the DB pool, the nar_storage backend, and the email transport (if enabled), and return a body that distinguishes "I'm running" from "all dependencies up".

### 13.6 [LOW] No audit log
No table records who deleted what. Stand-alone `audit_log (actor, action, target, before, after, ts)` would have caught the "viewer kicked the founder" bug class in §2.1 even if RBAC failed.

---

## 14. Dependencies & build

### 14.1 [MEDIUM] `crypter` crate version 0.3 — small/uncommon
File: `core/Cargo.toml:48`. 1.5k downloads on crates.io, single maintainer, minor version. Used to seal: SSH private keys, cache signing keys, webhook secrets, GitHub App keys. Pin a specific reviewed version OR migrate to a mainstream AEAD: `aes-gcm` from `RustCrypto`, `age`, or `aws-lc-rs`. Document the algorithm choice and KDF parameters in `docs/`.

### 14.2 [MEDIUM] `git-url-parse = "0.6"` declared in both `core` and `web`
DRY: once at workspace level.

### 14.3 [LOW] `ed25519-compact` and `ssh-key` (which has its own `Ed25519Keypair`) both pulled in
`sources/ssh_key.rs:11-15` uses `ed25519_compact::KeyPair::generate()` then converts to `ssh_key::Ed25519Keypair`. Use one library: `ssh-key` already has key generation under the `rand` feature. -1 dependency, -50 LOC.

### 14.4 [LOW] `axum-streams = "0.25"` and `axum = "0.8"` — major-version coupling
`axum-streams` is a small companion crate; check it's actually used (`grep` says it's only listed in `Cargo.toml`). If unused, drop it.

### 14.5 [LOW] `cargo deny` / `cargo audit` not visible in CI
A `deny.toml` and a CI gate eliminate most of the "is this dep maintained?" noise from manual review. Add both.

---

## 15. Schema additions (cumulative, supersedes §9)

```text
api_key (
    id          uuid pk,
    owned_by    uuid fk→user,
    name        text,
    key_prefix  text,    -- "GRADxxxxxxxx", first 8 chars displayed in UI
    key_hash    bytea,   -- argon2id(token, salt) ; not bare sha256
    salt        bytea,
    scopes      text[],  -- ['cache:read','build:trigger',...]
    revoked_at  timestamptz nullable,
    expires_at  timestamptz nullable,
    last_used_ip inet nullable,
    last_used_at timestamptz,
    created_at  timestamptz,
    managed     bool
)

oidc_identity (
    id        uuid pk,
    user      uuid fk→user,
    issuer    text,           -- from discovery.issuer
    sub       text,           -- IdP-stable subject id
    UNIQUE (issuer, sub)
)
-- merging users by email is forbidden; this is the only join key.

session (
    id          uuid pk,
    user        uuid fk→user,
    jti         text,          -- for revocation
    user_agent  text,
    ip          inet,
    created_at  timestamptz,
    expires_at  timestamptz,
    revoked_at  timestamptz nullable
)
-- backs `revoked_tokens`; logout writes revoked_at.

audit_log (
    id          uuid pk,
    actor_user  uuid nullable, -- null for system / webhook events
    actor_kind  text,          -- 'user' | 'api_key' | 'system' | 'webhook'
    action      text,          -- 'org.delete', 'project.create', ...
    target_kind text,
    target_id   uuid nullable,
    metadata    jsonb,
    created_at  timestamptz
)

webhook_delivery (
    id            uuid pk,
    webhook       uuid fk→webhook,
    event         text,
    event_id      uuid,         -- replay-id for idempotency
    request_body  bytea,        -- truncated
    signature     text,
    response_code int4 nullable,
    response_body bytea nullable,
    attempted_at  timestamptz,
    next_retry_at timestamptz nullable
)
```

---

## 16. Updated remediation order

1. **Org RBAC** (§2.1, §2.2, §11.9) — single biggest blast radius. Implement §12.1 `OrgRole::require` extractor; rip out `load_editable_org` / `load_org_member`.
2. **OIDC** (§1.1, §1.2, §1.3) — replace with the `openidconnect` crate (which handles `state`, `nonce`, JWKS verification, `iss`/`aud` correctly).
3. **Direct-build path traversal + size limit** (§5.1, §5.2, §7.3) — fold into the `MultipartForm` rewrite (§12.7).
4. **SSRF on outbound webhooks** (§4.1, §4.3, §11.4) — URL parser + RFC1918/loopback block + redirect policy + DNS pinning.
5. **API key model** (§1.6, §15) — new table, scopes, revocation, prefix-display.
6. **Secrets at rest** (§6.1, §11.5, §11.6) — load secrets once via `ArcSwap`; delete the SSH plaintext-base64 fallback; RAII `EphemeralKeyFile`.
7. **Lost-update on metrics** (§7.1) — atomic SQL UPDATE.
8. **Worker token comparison** (§11.1) — switch to `subtle::ConstantTimeEq`.
9. **JWT decode → 401** (§1.4); algorithm pinned (§1.5); `Cliams` → `Claims` (§1.12); `Principal` enum (§8.2).
10. **`BaseResponse` envelope removal** (§12.2) — large but mechanical.
11. **`EvaluationStatus::is_active`** (§12.3) — landed once, fixes 7 sites.
12. **`provisioning.rs` Upsert trait** (§12.4) — 1205 → ~400 LOC.
13. **Audit log + webhook deliveries** (§13.6, §15) — operational visibility.
14. Everything else under §12 — chip away as files are touched.

---

## 18. Rate limiting (and the lack of it)

**Top line:** there is no rate limiting anywhere in the HTTP layer. Searches for `tower-governor`, `RateLimit`, `Quota`, `governor::`, `tower::limit`, `ConcurrencyLimit` return zero hits in the workspace and zero in `Cargo.toml`. The nix modules expose no rate-limit knob either, so operators can't add one without editing the binary. The only resource throttle in the entire request path is the sea-orm DB pool (`max_connections=N`, `acquire_timeout=8s`) at `core/src/db/connection.rs:34-49` — that is *not* a rate limit, it's a fail-closed circuit-breaker that turns "too much traffic" into "every request 500s for 8 seconds at a time".

This means every route in `lib.rs` is, in practice, billed at "as fast as the network allows". Worker semaphores in `worker/src/worker_pool/pool.rs` and `max_concurrent_builds` in the scheduler are *worker-side* capacity controls — they don't shape inbound HTTP at all.

### 18.1 [HIGH] Login / OIDC flows have no per-IP or per-user throttle
File: `endpoints/auth.rs:140-186` (`post_basic_login`).

`verify_password` runs Argon2 (set in `password-auth` with the `argon2` feature, `auth.rs:20`) — typical cost ~50–100 ms per invocation, single-threaded CPU-bound. Without a rate limit:

- One attacker thread that POSTs `/auth/basic/login` with arbitrary credentials can pin one CPU core continuously by triggering Argon2 hashing (the lookup happens before, but a successful username match always hashes). A small bot farm flatlines the service.
- Username + password spraying is unbounded (combined with §1.10's username enumeration).
- `oidc/callback` triggers two `reqwest` round-trips per call (token exchange + userinfo); easy outbound amplifier.

Need: per-IP token-bucket on `/auth/basic/login` (e.g. 5 req / 5 min), exponential per-username lockout after N failures, CAPTCHA after N/IP/hour. The `tower-governor` crate is the standard tower middleware for this.

### 18.2 [HIGH] Email-verification endpoints have no throttle
Files: `auth.rs:356-455`.

- `post_resend_verification` posts to `state.email.send_verification_email(...)`. With no per-IP and no per-username cap, an attacker scripts `/auth/resend-verification` for every leaked email address in a breach corpus → free spam relay billed to the operator's SMTP credentials.
- `get_verify_email?token=...` is the only path that can flip `email_verified=true`. With 24-hour-validity tokens and no rate limit, the endpoint is a free brute-force oracle. (Token entropy needs verification — see §6.3 about general entropy floors; if `generate_verification_token` is a 16-char alphanumeric, that's only ~95 bits which is fine, but the rate limit is still the right defence.)

### 18.3 [HIGH] `user::get_search` is a global enumeration sink
File: `endpoints/user.rs:59-84`. Already flagged in §2.4. Adding rate-limit context: a single authenticated user can issue thousands of `?q=a`/`?q=ab`/... per second, walking the entire user table by varying prefix. `LIMIT 10` does not save you. Either scope to "users I share an org with" *and* cap at 5 req / 10s per user — or both.

### 18.4 [HIGH] Public NAR/cache endpoints are anonymous and unbounded
File: `lib.rs:265-271` mounts `/cache/{cache}/nar/{path}` etc. without `route_layer(authorize_optional)` or any limit. For a public cache, anyone on the internet can:

- Issue `nar` GETs at line rate. `endpoints/caches/nar.rs:39-43` reads the entire NAR into memory (`Vec<u8>`, see §3.3) before returning. Each GB-scale NAR served = ~GB of RAM held for the duration of the connection. A handful of slow-reading clients (bandwidth-throttled at the receiver) sit on multiple GB of RAM each.
- Hit `/cache/{cache}/nar/upstream/{upstream_id}/{*path}` (`endpoints/caches/nar.rs:56-82`) which proxies an outbound `reqwest::Client::new().get(nar_url).send()`. Each request creates a fresh `reqwest::Client` (no pool), opens a TCP/TLS connection, no timeout, no body size cap. Two trivial DoS vectors: outbound connection pool exhaustion + memory amplification when the upstream returns large bodies.

Need: bandwidth/connection caps per-IP on cache endpoints, and a streaming response (which 3.3 already calls for). Public caches should also opt into a sliding-window byte cap per IP (`X bytes / 5 min`) — the same protection a CDN front would apply.

### 18.5 [HIGH] Forge webhook endpoints are unauthenticated until HMAC verification
Files: `endpoints/forge_hooks/mod.rs:46-50, 115-120`. Even though we reject invalid signatures, the request body has already been read into `Bytes` (see §4.7), and the HMAC verification is `O(body length)`. An attacker who knows the routes (they're documented in `mod.rs:14-15`) hammers them with random bodies; the server burns CPU on HMAC + DB lookup of the integration row before rejecting. Combined with no body limit, this is a CPU + RAM amplification bug.

Need: hard 256 KB body limit on `/hooks/*`, per-IP request rate, and circuit-break the integration lookup if HMAC has failed N times in a row from the same source.

### 18.6 [HIGH] Outbound webhook delivery has no fan-out cap
File: `core/src/ci/webhook.rs:239-281`. `fire_webhooks` iterates every active webhook for an org and delivers in-line. An org can register 50 webhooks; on every build event, that's 50 sequential outbound HTTP requests held in the same task. A misconfigured set (slow receivers, 10s timeout each) blocks the calling task for 8+ minutes. Combined with §4.1, this is also an SSRF amplification vector (50 internal-IP probes per build).

Need: a global outbound-webhook concurrency semaphore (e.g. 100 parallel deliveries server-wide) and a per-org per-event cap (≤ 10 webhooks fan-out, log the rest). Use `tokio::sync::Semaphore` in a dedicated background queue instead of inline-in-handler delivery.

### 18.7 [HIGH] `post_direct_build` and `post_build_log` accept large bodies with no cap
Files: `endpoints/builds/direct.rs:38-71`, `endpoints/builds/log.rs:*`. Already covered in §5.2 / §7.3. Worth restating in this section: rate-limit + body-size-limit are the two layers of the same defence; you need both.

### 18.8 [MEDIUM] No global request timeout
There is no `tower_http::timeout::TimeoutLayer` on the router (`lib.rs:32-49` only adds CORS and trace). A handler that hangs (DB lock held by something else; reqwest call without inner timeout) keeps the connection and one tokio worker thread tied up forever. Add a 60s global deadline (with per-route opt-out for genuinely long endpoints like log streaming).

### 18.9 [MEDIUM] No connection-count limit
`axum::serve(listener, app)` does not impose `tower::limit::ConcurrencyLimit`. `Connection: keep-alive` from a sleeping client is free. SYN-flood / slow-loris is a real concern on a public deployment without a fronting reverse proxy. Add a `ConcurrencyLimitLayer` calibrated to (DB pool size × 2) at the router root; reject excess with 503 + `Retry-After`.

### 18.10 [MEDIUM] `outbound::connect_to_registered_workers` ticks every 15s with no jitter
File: `proto/src/outbound.rs:31-38`. `tokio::time::interval(Duration::from_secs(15))` — synchronised across all server replicas, so multi-instance deploys fan out simultaneously. Add jitter (`+/- 5s`) and a per-worker exponential backoff on connection failure.

### 18.11 [MEDIUM] DB pool is the *only* backpressure
`acquire_timeout=8s` (`core/src/db/connection.rs:48`) — when the pool saturates, every handler waits up to 8s and then returns 500 (`WebError::Database`). User-visible behaviour: the site goes from "fine" to "every page broken" with no graceful degradation, no `Retry-After`, no per-endpoint priority. A real rate-limit layer at the front means the DB pool only sees traffic the server has decided to admit.

Recommendations for the layer:

```rust
use tower::ServiceBuilder;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tower::limit::ConcurrencyLimitLayer;

let global = GovernorConfigBuilder::default()
    .per_second(50)
    .burst_size(100)
    .key_extractor(SmartIpKeyExtractor)  // honour X-Forwarded-For if --trust-proxy
    .finish().unwrap();

let auth = GovernorConfigBuilder::default()
    .per_second(1).burst_size(5)            // 5 attempts, recharge 1/s
    .finish().unwrap();

let webhook_inbound = GovernorConfigBuilder::default()
    .per_second(5).burst_size(20).finish().unwrap();

app
    .layer(TimeoutLayer::new(Duration::from_secs(60)))
    .layer(ConcurrencyLimitLayer::new(state.cli.max_in_flight as usize))
    .layer(RequestBodyLimitLayer::new(2 * 1024 * 1024))   // 2 MiB JSON default
    .layer(GovernorLayer { config: global.into() });

// Per-route harder limits via `route_layer(GovernorLayer { config: auth })`
// on `/auth/*`, `/hooks/*`, `/user/search`.
```

### 18.12 [MEDIUM] Operator surface — nix module exposes no rate-limit knobs
`nix/modules/gradient.nix` lets operators toggle `proto.federate`, `discoverable`, etc. but nothing about request rates, body sizes, or timeouts. Once the layer above lands, expose at minimum:

```nix
services.gradient = {
  rateLimit = {
    global = { perSecond = 50; burst = 100; };
    auth = { perSecond = 1; burst = 5; };
    webhook = { perSecond = 5; burst = 20; };
  };
  maxBodyBytes = mkOption { ... };
  requestTimeoutSeconds = mkOption { ... };
  trustProxyHeaders = mkOption { ... };  // for SmartIpKeyExtractor
};
```

The last one matters: behind nginx/Caddy/Cloudflare, you must trust `X-Forwarded-For` to derive the real client IP, otherwise the rate-limit is keyed on the proxy IP and is useless. Behind no proxy, you must NOT trust it (header-spoofing → bypass). One operator-toggle, default `false`.

### 18.13 [LOW] Per-account quota / fair-share
Beyond rate limits, the data model has no per-org / per-user quotas:

- Number of caches per org.
- Total NAR bytes stored per org.
- Number of evaluations per project per hour.
- Number of API keys per user.
- Number of webhooks per org.

A rogue tenant on a multi-tenant install can fill the disk or evaluation queue. Add a `quota` table and check at create time. Not a rate-limit per se, but adjacent.

### 18.14 [LOW] Test coverage for rate-limit behaviour
Once the layer lands, add integration tests that drive `axum_test::TestServer` past the threshold and assert `429 Too Many Requests` with a `Retry-After` header. Without tests, the limits silently regress on every refactor of the router.

---

## 17. What I still did NOT audit

- `worker/executor/eval.rs` (859 LOC), `worker/proto/nar_import.rs` (1104 LOC) — Nix sandboxing & content-address verification.
- `scheduler/dispatch.rs`, `scheduler/policy.rs` — scheduling fairness, starvation, evaluation isolation.
- `worker/connection_state.rs`, `proto/handler/cache.rs` (841 LOC) — proto state machines for malformed message robustness.
- `core/storage/nar_extract` — tar/zst path-traversal during extraction (since the NAR-to-tar archive path serves user-controlled content).
- `core/ci/reporter.rs` (825 LOC) — outbound CI status reporting; likely SSRF-adjacent.
- Frontend.
- `nix/modules/` and `nix/tests/` — operator-facing config surface.
- The `migration/` crate's downgrade paths (other than 5.8 / 5.9).
- License of dependencies (AGPL-only project, watch for accidentally-added GPL/non-GPL-compatible deps).
- Container image base, non-root user, capabilities.
- `journalctl`-visible secret leaks (does the eprintln in `load_secret` get captured?).

These are all viable seams for a third pass.

