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
| E | `caches/helpers.rs` and `proto/handler/cache.rs` both define a struct purely to thread `state` | 12.6 / 19.9 |
| F | Multipart parsing is hand-rolled instead of `#[derive(TryFromMultipart)]` (also fixes 3 security bugs) | 12.7 |
| G | `secret_file: String` cloned into every helper; `&str` is correct | 12.9 |
| H | `serve_url.replace("https://", "")` dance copy-pasted 4× | 12.14 |
| I | `WebError`'s 14 variants and helper soup should collapse to ~6 with `thiserror` and stable `error_code` | 12.10 / 12.11 |
| J | `pub use self::*::*;` in mod.rs files hides where things come from | 12.8 |
| K | **No transactions exist in non-test code** — multi-step DB writes are non-atomic | 19.1 |
| L | `Cli` is a 65-field god object — every handler depends on every feature flag | 19.2 |
| M | No newtype wrappers for IDs; `user_id`/`org_id` swap is a silent bug | 19.3 |
| N | `entity_aliases.rs` defines 160 single-letter prefixed type aliases | 19.4 |
| O | `web_db` vs `db` split has no type to defend it | 19.5 |
| P | Raw SQL with hard-coded enum integers in `dispatch.rs` (`status NOT IN (5,6,7)`) | 19.6 |
| Q | Migration 9 has a permanent typo: `build_depencdency` | 19.7 |
| R | 74 migrations, no squash strategy | 19.8 |
| S | Background `tokio::spawn` everywhere with no shutdown token | 19.26 |
| T | `wildcard` column stores three different semantics depending on flow | 19.13 |
| U | **N+1 query farm**: `evaluation_to_summary` issues 6 COUNT/SELECT calls × per-evaluation | 20.1 |
| V | **13 different "load X by name and access-check" functions** with subtly different shapes | 20.2 |
| W | **40 hand-written `if let Some` PATCH boilerplate blocks** — derive-able | 20.3 |
| X | **8 copies of `GRADIENT_CREDENTIALS_DIR` env-var fallback** in `provisioning.rs` | 20.4 |
| Y | **92 manual `Json(BaseResponse{})` constructions, 36 `find_by_id+ok_or_else` triples, 280 `WebError::*` constructions, 71 `Utc::now().naive_utc()` calls** | 20.5–20.8, 20.24 |
| Z | **19 reqwest clients constructed ad-hoc** — no shared HTTP client, no shared timeout/redirect | 20.7 |
| AA | **39 background `tokio::spawn` sites** with no `BackgroundJobs` registry, no shutdown | 20.9 |
| AB | **`as_i16`/`from_i16` hand-rolled** on every entity-stored enum (×~7) | 20.10 / 20.11 |
| AC | `Role` lives as 3 `Uuid` constants, not an enum with `PartialOrd` | 20.18 |
| AD | **Logger initialised AFTER `init_state`** — startup logs (incl. migration failures) silently dropped | 22.1 |
| AE | **Zero security-event audit logging** — login success/failure, role change, delete-org all silent | 22.4 |
| AF | **Printf-style logging dominates 8:1** over structured fields (`error = %e`) | 22.3 |
| AG | **`error!` overused for "row not found"** / user input — drowns real server errors | 22.5 / 22.6 |
| AH | **No request-id / span correlation** across handler → DB → spawned cleanup | 22.7 |
| AI | `eprintln!` in `load_secret` + worker subprocess bypasses tracing entirely | 22.2 / 22.8 |
| AJ | **`commits.rs` endpoint has TODO "check if user has access"** — IDOR | 23.2 |
| AK | `cli.max_proto_connections` declared but **unused** | 23.5 |
| AL | GitLab outbound CI reporter is a silent **no-op stub** | 23.4 |
| AM | API key revocation/scopes, JWT revocation on logout, audit log, webhook history — **all unimplemented** despite features around them | 23.4 |
| AN | `keep_evaluations` and `nar_ttl_hours` default to 0 = disabled — unbounded growth in default deploy | 23.12 / 23.13 |
| AO | **OIDC errors never logged** — silent on the journal, but echoed back in the HTTP body | 24.1 |
| AP | **`StateOrganization` has no `members` field** — operators can't manage org membership via state config | 24.2 |
| AQ | **No shutdown plumbing through `WorkerPoolResolver`** — eval-worker subprocesses always SIGKILL'd via `kill_on_drop` | 24.3 |
| AR | **`preferLocalBuild` parsed but discarded** — scheduler can't bias dispatch on it | 24.4 |
| AS | **Evaluations stuck in `Queued` when no compatible worker exists** — `reconcile_waiting_state` skips `Queued` rows | 24.5 |
| AT | **API returns no reason for `Waiting`** — info exists in tracing only, never on the row or in the response | 24.6 |
| AU | **Worker exits permanently on a single reconnect failure** — comment says "will retry", code says `break` | 24.7 |
| AV | **CI reporter not injectable in tests** — terminal-state assertions impossible with `MockDb` | 24.8 |
| AW | Transitive dependency cascade hard to test — graph walk + `MockDb` choreography don't compose | 24.9 |

### Cross-cutting refactors (each retires a half-dozen findings)

- §21.1 — `ResourceCtx<R, Min: Role>` extractor → folds in 9 findings.
- §21.2 — `BaseResponse` removal + typed `WebError` → folds in 5 findings.
- §21.3 — `BackgroundJobs` registry → folds in 11 findings.
- §21.4 — Strongly-typed config + DI seam → folds in 10 findings.
- §21.5 — `#[derive(Patch)]` proc macro → folds in 6 findings.
- §21.6 — `Crypter` service → folds in 8 findings.

### A reasonable first sprint

Each of these absorbs many individual findings — see §21 for the cross-cutting refactor map.

1. **`ResourceCtx`** (§21.1) — fixes the org RBAC class structurally. Kills §2.1, §2.2, §2.5, §2.6, §11.9, §12.1, §20.2, §20.18, §20.25 in one PR.
2. **OIDC rewrite** with `openidconnect` crate — fixes §1.1 / §1.2 / §1.3.
3. **Multipart rewrite** with `axum-typed-multipart` — fixes §5.1 / §5.2 / §5.3 / §12.7.
4. **SSRF guard** + shared `state.http: reqwest::Client` (§20.7) — closes §4.1, §4.3, §11.4, plus the 19 ad-hoc client constructions.
5. **Rate-limit / body-limit / timeout layer** (§18.11) + nix module knobs (§18.12) — shuts down §1.11, §4.7, §5.2, §7.3, §18.1–18.10 collectively.
6. **`Crypter` service** (§21.6) — fixes §6.1, §6.2, §6.3, §11.5, §11.6, §11.7.
7. **`BackgroundJobs` registry** (§21.3) — fixes §7.2, §7.4, §11.11, §13.1, §13.4, §18.10, §19.10, §19.26, §20.9, §20.36 plus the silent-DB-error sinks.
8. **`Json(BaseResponse)` purge + typed `WebError`** (§21.2) — mechanical, ~280 LOC saved, fixes the frontend coupling problem (§12.10, §12.11, §20.5, §20.24).

Everything else can be staged incrementally as files are touched. After 1–8 land, the codebase is roughly **~2.5k LOC smaller** and ~80% of the findings in this document close passively.

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

## 19. Deeper structural caveats (round 3)

### 19.1 [HIGH/STRUCTURAL] No transactions, anywhere, in non-test code
Search: zero hits for `TransactionTrait`, `begin_transaction`, or `db.begin()` outside tests. Multi-step DB operations are non-atomic across the entire codebase. Concrete leaks:

- `orgs/management.rs::put` (lines 257–280) — INSERT org → INSERT `organization_user(role=Admin)`. If the second insert fails (FK error, deadlock), the org exists with **no admin** and §2.1's "RBAC absent" bug becomes "no remediation possible without DB surgery".
- `auth.rs::post_basic_register` (lines 95–121) — INSERT user → send verification email. The email is best-effort with `tracing::warn!`; if SMTP is down the user has no path to verify and effectively can't log in (when `email_require_verification=true`).
- `builds/direct.rs::post_direct_build` (lines 117–161) — INSERT commit → INSERT evaluation → INSERT direct_build. Partial failure leaks rows. Worse: the `temp_dir` filesystem write happens *before* any DB write, so a DB error after the upload leaves bytes on disk forever (§5.3).
- `caches/management.rs::delete_cache` (lines 343–350) — DELETE cache row first, THEN spawn a tokio task to clean up NAR files. A server crash between the DB commit and the cleanup spawn leaks NAR files forever.
- `oidc.rs::create_or_update_user` (lines 189–230) — *four* sequential UPDATEs of the same row when several fields differ, each a separate DB round-trip and each its own commit. The user may observe a partial profile mid-flight.

The fix is one method on `ServerState`:

```rust
async fn with_txn<R>(&self, f: impl AsyncFnOnce(&DatabaseTransaction) -> Result<R>) -> Result<R>
```

…and a CI lint: any handler that performs ≥2 DB writes must take a `&DatabaseTransaction`.

### 19.2 [HIGH/STRUCTURAL] `Cli` is a 65-field god object
File: `core/src/types/mod.rs:42-234`. Concerns include logging (4 fields), networking (4), DB (2), evaluation/scheduler (4), filesystem (2), OIDC (5), email (8), GitHub App (3), S3 (6), TLS (1), federation (3), GC (2), proto (3), state-mgmt (2), and more. Every handler that needs *any* config takes `state.cli` and so transitively depends on every other feature flag.

What this prevents:
- Tests can't say "OIDC is enabled" without constructing a 65-field `Cli` (the `test-support::test_cli()` helper exists exactly because of this — but every new field needs to be threaded through).
- Reading `state.cli.serve_url` makes it look as if `serve_url` is mutable; it's not, but the type doesn't say so.
- Domain modules that should know nothing about email take `state` and so transitively touch `email_smtp_*`.

Refactor target:

```rust
pub struct Cli { /* parsed once, then split */ }
impl Cli {
    pub fn into_config(self) -> AppConfig { ... }
}

pub struct AppConfig {
    pub net: NetConfig,         // ip, port, serve_url, use_tls, quic, frontend_url
    pub log: LogConfig,         // 4 log_level fields
    pub db:  DbConfig,
    pub eval: EvalConfig,       // max_concurrent_*, evaluation_timeout, eval_workers, ...
    pub oidc: Option<OidcConfig>,
    pub email: Option<EmailConfig>,
    pub github_app: Option<GitHubAppConfig>,
    pub s3: Option<S3Config>,
    pub gc: GcConfig,
    pub federation: FederationConfig,
    pub auth: AuthConfig,       // jwt_secret, crypt_secret, registration, role flags
}
```

Each handler then takes only what it needs. The `Option<XConfig>` shape (already partially present — `OidcConfig::oidc_config()`, `GitHubAppConfig::github_app_config()`) becomes the universal pattern.

### 19.3 [HIGH/STRUCTURAL] No newtype wrappers for any ID — `Uuid` is overloaded
Search: `pub struct .*Id(` returns zero hits in `core/src/types/`. Every entity primary key, every foreign-key field, every function parameter, every WS message field is bare `Uuid`. Consequence:

```rust
pub async fn user_is_org_member(state: &Arc<ServerState>, user_id: Uuid, organization_id: Uuid) -> ...
```

A caller can swap the two arguments and the compiler is silent. The codebase is full of `(user_id, organization_id, project_id, build_id, evaluation_id, derivation_id, cache_id, ...)` cascades — all `Uuid`. The *single* concrete bug class this prevents was already seen (§2.1 caller could pass `org_id` where `user_id` was expected; the function would silently look up a non-existent membership and fail closed). Not a security bug today, but the type system is screaming for help.

```rust
#[derive(Copy, Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct UserId(Uuid);
pub struct OrgId(Uuid);
pub struct ProjectId(Uuid);
pub struct CacheId(Uuid);
// ...
```

…with one macro to generate them. Costs: ~200 LOC at definition, ~0 LOC at call sites (everything still compiles). Eliminates the entire "id-swap" footgun class permanently.

### 19.4 [HIGH/STRUCTURAL] `entity_aliases.rs` defines 160 single-letter prefix aliases
File: `core/src/types/entity_aliases.rs`. For every one of 32 entities, the file aliases:

- `EApi` (Entity), `MApi` (Model), `AApi` (ActiveModel), `CApi` (Column), `RApi` (Relation).

…times 32 entities = 160 aliases. The intent is to type 5 chars instead of `api::Entity` (12 chars). Costs:

- `CCachedPathSignature` is now a real type the team has to live with.
- Grep for `Entity` returns 0 results across the entire codebase even though every handler uses sea-orm. Discoverability is destroyed.
- Beginner-hostile: a new contributor cannot read `EUser::find().filter(CUser::Username.eq(...))` and know what library that's from without `cargo doc` or jumping through `mod.rs`.
- Sea-orm's documentation, examples, and tutorials all use `user::Entity` / `user::Column`. Following any external reference requires mental translation.

Delete the aliases. Use the generated names. The keystroke cost is real (12 chars vs 5 chars × tens of thousands of uses) but pays for itself in onboarding and grep-ability. If the team really wants brevity, `use entity::user::{Entity as UserEntity, Column as UserCol};` at the file top is the same brevity scoped to one file.

### 19.5 [HIGH/STRUCTURAL] `web_db` vs `db` split is a load-bearing implementation detail with no type to defend it
File: `core/src/types/mod.rs:237-251`. The `ServerState` has `db: DatabaseConnection` and `web_db: DatabaseConnection`. The doc comment says: "Dedicated DB pool used by the axum/web layer so HTTP requests are not starved by the busy proto/scheduler pool under heavy NarPush load."

Problems:
- The names are not self-documenting (`web_db` vs `db` — what's `db`?).
- There is no compile-time enforcement of which queries belong to which pool; mixing them silently negates the segmentation.
- One handler (`endpoints/caches/nar.rs:124`) actually uses `state.db` — was that intentional or a bug? The type can't say.
- For replicas / read-only mirrors / write-followers, this 2-pool model breaks down completely.

Better:

```rust
pub struct AppDb {
    pub user_facing: DatabaseConnection,
    pub background:  DatabaseConnection,
}
impl AppDb {
    pub fn for_request(&self) -> &DatabaseConnection { &self.user_facing }
    pub fn for_background(&self) -> &DatabaseConnection { &self.background }
}
```

…and never expose the raw fields. Even better: separate `ReadDb` and `WriteDb` types when read-replica support arrives.

### 19.6 [HIGH/STRUCTURAL] Raw SQL with hard-coded enum integers in `scheduler/dispatch.rs`
File: `backend/scheduler/src/dispatch.rs:75` (and surrounding):

```rust
let sql = sea_orm::Statement::from_string(
    sea_orm::DbBackend::Postgres,
    // Terminal statuses: 5=Completed, 6=Failed, 7=Aborted.
    "...status NOT IN (5, 6, 7)..."
);
```

The comment is doing the work the type system should. `EvaluationStatus` already exists as a typed enum in `entity/`; the SQL hand-codes its discriminants. If anyone reorders the variants (a `#[derive(EnumIter)]` accident, an inserted variant), this query silently selects the wrong rows. Use `Status.is_in([Completed, Failed, Aborted])` via sea-query, or generate the integers from the enum at runtime.

This finding pairs with §12.3: an `EvaluationStatus::is_active()` method would let the comment go away entirely.

### 19.7 [HIGH/STRUCTURAL] Migration 9 has a permanent typo: `m20241107_155000_create_table_build_depencdency`
File: `backend/migration/src/m20241107_155000_create_table_build_depencdency.rs`. The migration name `build_depencdency` is `derive(DeriveMigrationName)` so the *typo is the canonical migration ID stored in `seaql_migrations`*. Renaming the file/struct now would cause sea-orm to consider it a new migration on running installs (re-running the create-table → error). This is permanent debt unless someone writes a manual `UPDATE seaql_migrations SET name = ...` migration.

Lift to a process check: a CI `cargo spellcheck` over `migration/src/` plus a strict naming policy (`mYYYYMMDD_NNNNNN_<verb>_<noun>` with verbs in a closed set: `create_table` / `add_X_to_Y` / `drop_X_from_Y` / `rename_X_to_Y`).

### 19.8 [HIGH/STRUCTURAL] 74 migrations, no squash strategy
File: `backend/migration/src/lib.rs` declares 74 `mod m...;` lines, with another listed but unmerged (`m20260502_000000_hash_api_keys`). Several pairs cancel each other (`add_has_artefacts_to_build_output` → `drop_has_artefacts_from_derivation_output`; `add_github_app_enabled_to_organization` → `drop_github_app_enabled_from_organization`). New installs run all 74; the up-front cost is small but rises monotonically.

Pre-1.0 cutover convention: at a chosen release, freeze the schema as a single `m_baseline_v1.rs` that creates the current DB state, mark `delete_state` migrations as compatible, and gate older installs through an explicit `cargo run -- migrate from-v0`. Downgrade paths are usually a no-op for projects without prod customers, but document this explicitly.

### 19.9 [HIGH/STRUCTURAL] `proto/handler/cache.rs::CacheQueryHandler` repeats the `state`-threading anti-pattern
File: `backend/proto/src/handler/cache.rs:17-23`. Same as §12.6:

```rust
struct CacheQueryHandler<'a> {
    state: &'a ServerState,
}
impl<'a> CacheQueryHandler<'a> {
    fn new(state: &'a ServerState) -> Self { Self { state } }
    ...
}
```

A struct that exists *only* to thread `state` (and outer impl wrappers around `&Arc<ServerState>` already exist for free). Same fix as §12.6 — pick free functions or pick a meaningful struct, not both.

### 19.10 [HIGH/STRUCTURAL] `start_dispatch_loops` triplicate spawn pattern
File: `backend/scheduler/src/dispatch.rs:43-49`:

```rust
let s1 = Arc::clone(&scheduler);
let s2 = Arc::clone(&scheduler);
let s3 = Arc::clone(&scheduler);
tokio::spawn(async move { project_poll_loop(s3).await });
tokio::spawn(async move { eval_dispatch_loop(s1).await });
tokio::spawn(async move { build_dispatch_loop(s2).await });
```

…and each `*_loop` is a copy-paste of:

```rust
async fn X_loop(scheduler: Arc<Scheduler>) {
    let mut interval = tokio::time::interval(Duration::from_secs(N));
    info!("X loop started");
    loop {
        interval.tick().await;
        if let Err(e) = poll_X(&scheduler).await { error!(error = %e); }
    }
}
```

Lift:

```rust
fn spawn_periodic<F, Fut>(name: &'static str, scheduler: Arc<Scheduler>, period: Duration, f: F)
where F: Fn(Arc<Scheduler>) -> Fut + Send + Sync + 'static,
      Fut: Future<Output = anyhow::Result<()>> + Send,
{
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        info!(loop = name, "started");
        loop {
            interval.tick().await;
            if let Err(e) = f(Arc::clone(&scheduler)).await {
                error!(loop = name, error = %e, "tick failed");
            }
        }
    });
}
```

…then `start_dispatch_loops` becomes 6 lines and adds free shutdown / metrics / jitter knobs in one place (also addresses §11.11 and §18.10).

### 19.11 [MEDIUM/STRUCTURAL] `nar_extract::extract_path_from_reader` allocates `read_to_end` without an upper bound
File: `core/src/storage/nar_extract.rs:146-148, 156-158`

```rust
let cap = std::cmp::min(size, MAX_PREALLOC as u64) as usize;
let mut buf = Vec::with_capacity(cap);
reader.read_to_end(&mut buf).await?;
```

`MAX_PREALLOC = 16 MiB` only caps the *initial* `Vec` capacity. `read_to_end` grows without limit. A NAR whose header claims `size=1MB` but whose body keeps streaming until `MAX_NAR_SIZE` lets the buffer balloon to whatever the producer wants. For server-uploaded NARs the producer is trusted (the worker), but trust within a federated install is the wrong default.

Fix: replace `read_to_end` with `tokio::io::AsyncReadExt::take(size).read_to_end(...)`. Now the reader truncates to the declared size, and a malformed NAR is `Err` not `OOM`.

Same bug for the directory collector (line 156-158).

### 19.12 [MEDIUM/STRUCTURAL] Foreign-key cascade behaviour is per-table, undocumented
Search: 79 `on_delete`/`on_update` clauses across 61 `ForeignKey::create` invocations. Some FKs have explicit cascades, some don't (defaults to `RESTRICT`). The `delete user` flow (§2.5) relies on cascades to clean up everything from `organization_user` to `api_key` to `evaluation_message`. There is no central documentation of which FK does what on parent delete.

Two recommendations:
1. CI lint: every `ForeignKey::create()` must explicitly set `.on_delete(...)`. Default-RESTRICT is a footgun when the codebase calls `entity.delete()` casually.
2. A small `docs/data-model.md` enumerating per-table delete behaviour. Best generated from the schema; sea-orm doesn't but a small `dot`-printer could.

### 19.13 [MEDIUM/STRUCTURAL] Naming inconsistency: `wildcard` field stores three different things
- For project-driven evals: a wildcard derivation pattern (`hostNixosConfigurations.*.config.system.build.toplevel`).
- For direct builds: the literal `.drv` path the user submitted (`/nix/store/abc-foo.drv`).
- The CLI flag is also named `evaluation_wildcard` on `Project` but `wildcard` on `Evaluation`.

Three semantics, one column, inconsistent naming. Refactor to:

```rust
enum EvalTarget {
    Wildcard(String),       // project flow
    DerivationPath(String), // direct build flow
}
```

Stored as `eval_target_kind: i16, eval_target_value: String` or as a JSON column.

### 19.14 [MEDIUM/STRUCTURAL] `record_nar_traffic` per-minute bucketing is wall-clock-only — DST/leap second hazard
File: `endpoints/stats.rs:67-73`:

```rust
let now = Utc::now().naive_utc();
let bucket = match now.with_second(0).and_then(|t| t.with_nanosecond(0)) { ... };
```

UTC has no DST so DST itself isn't a hazard. But: the `with_second(0)` fallback (`.unwrap_or(now)`) silently breaks bucketing if `with_second` ever returns `None` (it can't for `Utc::now()`, but the pattern is brittle). Bigger structural issue: minute-bucket lookups by `BucketTime.eq(bucket)` race with simultaneous inserts (§7.1). Pivot to event-time at the metric writer side, do bucket roll-up in a single batched background task.

### 19.15 [MEDIUM/STRUCTURAL] `core::types` mixes "types" and "logic"
File: `backend/core/src/types/mod.rs`. Inside `types/`:

- `consts.rs` — UUID constants.
- `input.rs` — *validation logic* (`load_secret`, `validate_username`, `check_index_name`).
- `wildcard.rs` — the wildcard parser (513 LOC of logic!).
- `secret.rs` — `SecretString`/`SecretBytes` wrappers (logic-bearing).
- `proto.rs` — message types (genuine types).
- `entity_aliases.rs` — type aliases (genuine types).

A module called `types/` should not contain a 513-LOC parser. Lift `wildcard.rs` to `core::wildcard`; `input.rs` to `core::validation` plus `core::secret_loader` (split per §6.1's concern); leave `types/` for *types*.

### 19.16 [MEDIUM/STRUCTURAL] `core/src/lib.rs` entrypoint hides what's exported
The crate is named `core`, which shadows `::core` (stdlib). Already noted in CLAUDE.md as "critical gotcha". Ideal: rename the package to `gradient_core` (and the dependency rename trick already in `test-support/Cargo.toml`/`builder/Cargo.toml`) — apply uniformly across the workspace. Removes an entire footgun category for any future async-trait / macro use.

### 19.17 [MEDIUM/STRUCTURAL] `lib.rs::create_router` constructs the router by appending strings of routes
File: `web/src/lib.rs:78-249` is essentially a 170-line procedural list of `.route("...", method(handler))` calls with hand-grouped `// ── ... ───` separators. Mechanical but invisible to tooling: there's no way to enumerate "all routes" or "all admin routes" programmatically.

A more declarative shape:

```rust
trait DomainRouter { fn router() -> Router<Arc<ServerState>>; fn auth_kind() -> AuthKind; }

impl DomainRouter for orgs::Module { ... }
impl DomainRouter for caches::Module { ... }

let api = AuthMiddleware::wrap(state, [
    orgs::Module::router(),
    caches::Module::router(),
    builds::Module::router(),
    ...,
]);
```

…or stay procedural but factor each domain's routes into its own module's `routes()` function, so `lib.rs::create_router` is ~30 lines and the route table for each domain lives next to its handlers.

### 19.18 [MEDIUM/STRUCTURAL] `proto::messages` exposes both types AND business logic
The proto crate has `handler/`, `messages/`, `outbound.rs`, `traits.rs` — but `proto/src/lib.rs` re-exports `Scheduler` from the `scheduler` crate "for backward compatibility". Layering inversion: `proto` depends on `scheduler` and re-exports its API. Either keep proto a pure protocol crate (only message types + WS framing) and have `web` depend on both, or merge them. The current "proto re-exports scheduler" hides a circular conceptual dependency.

### 19.19 [MEDIUM/STRUCTURAL] Handlers use `Extension(Arc<Scheduler>)` for one route family but `State` for everything else
The router merges `axum::Extension(scheduler)` (`lib.rs:255`) with `with_state(state)` (`lib.rs:282`). Two extractor families for two pieces of shared state. Pick one — `with_state((state, scheduler))` (or a tuple struct) and use everywhere.

### 19.20 [MEDIUM/STRUCTURAL] Workers register their *configuration* as part of the protocol (`max_concurrent_builds`)
File: `web/src/endpoints/orgs/workers.rs:104, 217` and `scheduler/src/worker_state.rs:67`. The worker advertises its capacity at handshake; the server trusts and stores it. A malicious or misconfigured worker can claim arbitrary parallelism, monopolise the dispatcher's eval queue, then never actually build. Server should clamp worker-advertised values to operator-configured per-org limits, not trust them.

### 19.21 [LOW/STRUCTURAL] The `builder` and `evaluator` crates exist on disk but are NOT in the workspace
Per the project-memory note: `evaluator` and `builder` "exist on disk but are NOT in workspace — they contain old server-side eval/build logic now superseded by the scheduler+worker architecture, BUT they still contain important logic not yet ported". This is dead code that is also load-bearing for future ports — schroedinger's crate. Either:

- Port the remaining logic and delete the directories.
- Mark them clearly with a `DEPRECATED-zombie/` directory rename.
- Keep them in the workspace but behind `#[cfg(feature = "legacy")]`.

Right now `find` returns `.rs` files that no `cargo check` ever touches; refactors silently miss them, then break when someone re-inducts them.

### 19.22 [LOW/STRUCTURAL] `entity` crate has 32 files but no per-domain grouping
File: `backend/entity/src/`. 32 sibling `*.rs` files. Sea-orm 1.x supports nested module entities; group by domain (`entity/src/cache/{cache.rs, cache_upstream.rs, cached_path.rs, cached_path_signature.rs, cache_metric.rs, cache_derivation.rs}`) so the compile-time impact of touching one entity scopes to its module.

### 19.23 [LOW/STRUCTURAL] `Send +Sync +Debug + 'static` bound copy-paste on traits
`core/src/ci/webhook.rs:27` (`WebhookClient`), `core/src/storage/log.rs`, `core/src/storage/email.rs` likely repeat the same `Send + Sync + Debug + 'static` quartet on every dyn-compatible trait. Define a `pub trait DepBound: Send + Sync + Debug + 'static {}` and `impl<T: ...> DepBound for T {}` once; then traits become `pub trait WebhookClient: DepBound { ... }`.

### 19.24 [LOW/STRUCTURAL] No dependency-injection layer; `ServerState` is the universal arg
Every handler takes `state: State<Arc<ServerState>>`. Every helper takes `state: &Arc<ServerState>`. Scheduler does the same. There is no notion of "this handler only needs the cache layer" — passing the full state is the lowest-friction option.

A small DI seam:

```rust
trait ProvidesCache { fn cache(&self) -> &dyn CacheService; }
trait ProvidesEmail { fn email(&self) -> &dyn EmailSender; }
impl ProvidesCache for ServerState { ... }
impl ProvidesEmail for ServerState { ... }

async fn handler<S: ProvidesCache>(s: &S, ...) -> ... { s.cache().get(...).await }
```

…lets tests inject a fake without rebuilding all of `ServerState`. A small refactor; pays back at every test site.

### 19.25 [LOW/STRUCTURAL] `BuildAccessContext` and `EvalAccessContext` are siblings but not unified
Files: `endpoints/builds/mod.rs:31-125`, `endpoints/evals/mod.rs::EvalAccessContext`. Both walk evaluation → project/direct → org → access-check, with subtly different shapes. Lift to one `WorkspaceContext` that resolves `(org, project_or_direct, evaluation, build?)` once, with field-level `Option<>`. Every "load build/eval/project" entry point becomes one method.

### 19.26 [LOW/STRUCTURAL] `Sometime later` background tasks have no shutdown handle
`tokio::spawn` is sprinkled across the codebase (`outbound.rs::start_outbound_loop`, `dispatch.rs::start_dispatch_loops`, `caches/management.rs::delete_cache` cleanup, `nar.rs::spawn_*_metric`, etc). There is no `Shutdown` token, no `JoinSet`, no graceful drain. Sending `SIGTERM` to the server abandons in-flight cleanups, in-flight metric writes, in-flight webhook deliveries. For prod: wire a `tokio_util::sync::CancellationToken` through `ServerState`, plumb it into every spawned loop, and `axum::serve(...).with_graceful_shutdown(token.cancelled())`.

### 19.27 [LOW/STRUCTURAL] Tests reach into private internals via `Arc::try_unwrap`
File: `proto/src/handler/auth.rs:264, 452-453`:

```rust
let state = Arc::try_unwrap(test_support::prelude::test_state(db)).unwrap();
state.cli.federate_proto = federate_proto;
```

The shape of test scaffolding is fighting the production API. Either expose a `TestStateBuilder` in `test-support` that returns a mutable struct, or accept that tests need an `Arc<ServerState>` and engineer the tests around it (e.g., `Cli` builder with `&mut self`).

### 19.28 [LOW/STRUCTURAL] `chrono::NaiveDateTime` used for DB-side timestamps; `chrono::DateTime<Utc>` for "current time"
Most rows use `created_at: NaiveDateTime` (because postgres `TIMESTAMP WITHOUT TIME ZONE`). Comparisons require `Utc::now().naive_utc()` everywhere — 30+ call sites. With `TIMESTAMP WITH TIME ZONE` and `DateTime<Utc>` consistently, the `.naive_utc()` calls disappear, the columns survive timezone-misconfigured DBs, and you stop storing implicit timezone info.

Change is a column-by-column migration; usually deferred until pre-1.0 cutover.

### 19.29 [LOW/STRUCTURAL] The eight `*_available` endpoints duplicate the same shape four times
- `get_org_name_available`
- `get_cache_name_available`
- `get_project_name_available`
- `post_check_username`

All do "validate name + check uniqueness + return bool" with three lookup variants. Lift into `endpoints/availability.rs` with one helper that takes a closure:

```rust
async fn name_available<F, Fut>(name: &str, validate: impl FnOnce(&str) -> Result<...>, exists: F) -> WebResult<bool>
where F: FnOnce(String) -> Fut, Fut: Future<Output = WebResult<bool>> { ... }
```

…then each handler is a 3-liner.

### 19.30 [LOW/STRUCTURAL] No `#[track_caller]` on `WebError` constructors
File: `web/src/error.rs:138-192`. Helpers like `WebError::not_found("Project")` panic-free, but when these get wrapped in `Internal(anyhow::Error)` paths, `tracing::error!` attaches the file/line of the helper, not the caller. `#[track_caller]` propagates the original site. Five-character change, free debuggability.

### 19.31 [LOW/STRUCTURAL] `JsonRejection` is a leaf variant in `WebError` but only ever maps to 400
File: `web/src/error.rs:30, 113-115`. `WebError::JsonParsing(JsonRejection)` exists as its own variant just to format `format!("Invalid JSON: {}", err)` — could be subsumed under `BadRequest` with a `From<JsonRejection>` shim. One less variant, same UX.

### 19.32 [LOW/STRUCTURAL] `Paginated<T>` and `PaginationParams` exist but only ~6 endpoints use them
Files: `core/src/types/mod.rs:259-280`. The pagination helper exists but `cache::get`, `user::get_keys`, and most "list" endpoints fetch unbounded rows with `// TODO: Implement pagination` comments. Either delete `Paginated` until used or actually use it everywhere. Half-finished pagination is worse than none.

### 19.33 [NIT/STRUCTURAL] `pub use` overflow in `ServerState`-related modules
e.g. `core/src/types/mod.rs:23-29`. Six `pub use` lines export over 200 items. Either every consumer does `use core::types::*;` (and they do) which collapses every type into one namespace (footgun for collisions), or they import individually (which they don't because the alias names already live in `core::types::*`). Pick one: explicit re-exports, never `*`.

### 19.34 [NIT/STRUCTURAL] `mod tests` at the bottom of every file vs `tests/` directory
The codebase mixes `#[cfg(test)] mod tests {}` inside source files (`jwt.rs`, `caches/nar.rs`, ...) AND a separate `web/tests/` directory (`forge_hooks.rs` at 624 LOC). No documented convention for which goes where. Pick: unit tests inline, integration tests in `tests/`. Move violators.

---

## 20. DRY violations — the long list

A scan with `grep` quantified concrete copy-paste counts across the workspace. Each entry below is a *measured* duplication, not a guess. Organised roughly by impact.

### 20.1 [HIGH/DRY] N+1 query farm in `evaluation_to_summary`
File: `web/src/endpoints/projects/evaluations.rs:42-119`. Each call issues **6 separate COUNT/SELECT queries** against the DB:

1. `ECommit::find_by_id(...)` for the commit hash.
2. `EBuild::count(...)` for total builds.
3. `EBuild::count(...)` for failed builds.
4. `EEntryPoint::find(...)` for entry points.
5. `EBuild::count(...)` for completed entry-point builds.
6. `EBuild::count(...)` for failed entry-point builds.
7. `EEntryPoint::count(...)` for the previous-evaluation diff.

It is then called *inside a loop* (lines 199, 226) over a paginated set of evaluations. Listing 50 evaluations triggers ~300 round-trips. The fix is one query with conditional aggregates:

```sql
SELECT
    e.id, e.commit, e.status, e.created_at, e.updated_at,
    count(b.*)                                 AS total_builds,
    count(b.*) FILTER (WHERE b.status = 6)     AS failed_builds,
    count(ep.build) FILTER (WHERE b.status IN (5, 8)) AS completed_eps,
    count(ep.build) FILTER (WHERE b.status = 6) AS failed_eps
FROM evaluation e
LEFT JOIN build b   ON b.evaluation = e.id
LEFT JOIN entry_point ep ON ep.evaluation = e.id AND ep.build = b.id
WHERE e.id = ANY($1::uuid[])
GROUP BY e.id;
```

(With the magic numbers replaced by `EvaluationStatus::is_active()`-style helpers per §19.6.) Even better: project the summary into a materialized view or a `evaluation_summary` cached table updated by triggers — the data only changes when builds change, and the dashboard hits this hot path on every page render.

### 20.2 [HIGH/DRY] 13 different "load X by name and access-check" functions
A grep for resource loaders found:

- `load_org_member`, `load_editable_org` (`endpoints/orgs/mod.rs`)
- `load_project`, `load_editable_project`, `load_readable_project` (`endpoints/projects/mod.rs`)
- `load_editable_cache`, `load_subscribable_cache` (`endpoints/caches/management.rs`, `endpoints/orgs/settings.rs`)
- `load_webhook_org`, `load_webhook` (`endpoints/webhooks.rs`)
- `load_integration` (`endpoints/orgs/integrations.rs`)
- `require_write_permission` (`endpoints/orgs/settings.rs`)
- `require_superuser` (`error.rs`)
- `user_can_edit`, `user_is_org_member` (`endpoints/projects/mod.rs`, `endpoints/mod.rs`)

13 functions. Each combines lookup + access check + state-managed flag + role check in a slightly different mix. One unified ladder:

```rust
struct ResourceCtx<R> { user: MUser, org: MOrganization, resource: R, role: Role }

impl<R> ResourceCtx<R> {
    async fn require(state: &ServerState, user: &MUser, lookup: ResourceLookup<R>, min: Role)
        -> WebResult<Self> { ... }
}
```

Every endpoint becomes one call:

```rust
let ctx = ResourceCtx::require(&state, &user, ResourceLookup::Cache(cache_name), Role::Write).await?;
```

…with `ResourceLookup` an enum covering Org/Project/Cache/Webhook/Integration. The state-managed check, the role check, and the existence check all live in one place — no more "I forgot to check role on `delete_organization_users`".

### 20.3 [HIGH/DRY] PATCH-handler boilerplate is 40 hand-written `if let Some(...)` blocks
Search: 40 hits of `if let Some(<field>) = body.<field>` in `endpoints/`. Every PATCH handler does:

```rust
let mut active: AThing = thing.into_active_model();
if let Some(name) = body.name {
    if check_index_name(&name).is_err() { return Err(...); }
    if existing_clash(&name).await? { return Err(...); }
    active.name = Set(name);
}
if let Some(display_name) = body.display_name {
    let trimmed = display_name.trim().to_string();
    if let Err(e) = validate_display_name(&trimmed) { ... }
    active.display_name = Set(trimmed);
}
if let Some(description) = body.description { active.description = Set(description.trim().to_string()); }
if let Some(priority) = body.priority { active.priority = Set(priority); }
...
```

Repeated in `caches/management.rs::patch_cache`, `orgs/management.rs::patch_organization`, `projects/management.rs::patch_project`, `webhooks.rs::patch_webhook`, `orgs/integrations.rs::patch_integration`, `user.rs::patch_settings`. The shape is mechanical: optional input field → validator → conflict check → setter.

A derive-driven approach:

```rust
#[derive(Deserialize, Patch)]
#[patch(model = "AOrganization")]
struct PatchOrganizationRequest {
    #[patch(validate = "check_index_name", unique_in = "organization::Name")]
    name: Option<String>,
    #[patch(trim, validate = "validate_display_name")]
    display_name: Option<String>,
    #[patch(trim)]
    description: Option<String>,
}

// handler
let active = body.apply_to(active);
```

…removes most of the file. (`derive_more::Patch` doesn't quite fit; this would be a project-local proc macro, but the cost of writing it pays back across 6+ endpoints.)

### 20.4 [HIGH/DRY] 8 copies of `GRADIENT_CREDENTIALS_DIR` env-var fallback
File: `core/src/state/provisioning.rs:119, 125-126, 188-189, 358-359, 576-577, 627-...` and beyond — the lines

```rust
let credentials_dir = std::env::var("GRADIENT_CREDENTIALS_DIR")
    .unwrap_or_else(|_| "/run/credentials/gradient-server".to_string());
let key_path = format!("{}/gradient_<thing>_{}_<kind>", credentials_dir, name);
let key = fs::read_to_string(&key_path).map_err(...)?;
```

…appear 8 times. Lift to:

```rust
struct CredentialsDir(PathBuf);
impl CredentialsDir {
    fn from_env() -> Self {
        Self(PathBuf::from(std::env::var("GRADIENT_CREDENTIALS_DIR")
            .unwrap_or_else(|_| "/run/credentials/gradient-server".into())))
    }
    fn read(&self, kind: &str, name: &str) -> Result<String, ...> {
        let path = self.0.join(format!("gradient_{kind}_{name}_private_key"));
        fs::read_to_string(&path).context(...)
    }
}
```

…and use once in `apply_state_to_database`.

### 20.5 [HIGH/DRY] 92 manual `Json(BaseResponse { error: false, message: ... })` constructions
Already noted in §12.2. The actual count: **92** in `web/src/endpoints/`. Each is a manual 3-line block. Removing the envelope (return `Json(T)` directly) deletes ~280 LOC of pure boilerplate.

### 20.6 [HIGH/DRY] 36 `find_by_id(_).one(&state.web_db).await?.ok_or_else(...)` triples
Search: 36 sites in `web/src/endpoints/`. The shape is invariant:

```rust
EThing::find_by_id(id)
    .one(&state.web_db)
    .await?
    .ok_or_else(|| WebError::not_found("Thing"))?
```

Lift to an extension trait on every entity:

```rust
trait LoadOrNotFound: EntityTrait {
    fn label() -> &'static str;
}
async fn load_or_404<E: LoadOrNotFound>(db: &DbConn, id: Uuid) -> WebResult<E::Model> {
    E::find_by_id(id).one(db).await?.ok_or_else(|| WebError::not_found(E::label()))
}
```

`load_or_404::<EProject>(db, id).await?` — 1 line, no English string at the call site.

### 20.7 [HIGH/DRY] 19 reqwest clients constructed ad-hoc
Search: 19 hits of `reqwest::Client::new()` / `reqwest::ClientBuilder::new()`. Each:

- creates a fresh TCP/TLS pool;
- has its own (or no) timeout policy;
- has its own (or no) redirect policy;
- doesn't participate in tracing or rate-limiting;
- defeats reqwest's connection-pool design.

Add `state.http: Arc<reqwest::Client>` constructed once in `init_state` with:

```rust
reqwest::ClientBuilder::new()
    .timeout(Duration::from_secs(30))
    .redirect(Policy::limited(3))
    .pool_max_idle_per_host(8)
    .user_agent(concat!("gradient/", env!("CARGO_PKG_VERSION")))
    .build()?
```

…and pass `&state.http` to every helper that needs HTTP. Combined with §4.1's SSRF guard, this is the single chokepoint for outbound safety.

### 20.8 [HIGH/DRY] 71 `Utc::now().naive_utc()` calls
Wrap once, see §12.28. Lift `fn now() -> NaiveDateTime { Utc::now().naive_utc() }` to `core::types`. Lets tests inject a `MockClock` later.

### 20.9 [HIGH/DRY] 39 background `tokio::spawn` sites with no shared infrastructure
Search: 39 spawns in non-test code. Six common idioms emerge:

- "Spawn an interval loop that polls the DB" (§19.10).
- "Spawn a fire-and-forget metric write" (`stats.rs`, `nar.rs`).
- "Spawn an outbound webhook delivery" (`ci/webhook.rs`).
- "Spawn a background cleanup after a delete" (`caches/management.rs`).
- "Spawn an outbound worker connect attempt" (`outbound.rs`).
- "Spawn a JWT-decoded background task" (`auth/middleware.rs`).

Each variant reinvents tracing, error-handling, and shutdown coordination. Lift to a `BackgroundJobs` registry on `ServerState`:

```rust
pub struct BackgroundJobs {
    join_set: Mutex<JoinSet<()>>,
    shutdown: CancellationToken,
}
impl BackgroundJobs {
    pub fn spawn_named(&self, name: &'static str, fut: impl Future<Output = ()> + Send + 'static) { ... }
    pub fn spawn_interval(&self, name: &'static str, period: Duration, body: impl Fn() -> Fut) { ... }
    pub async fn shutdown(&self) { ... }
}
```

Solves §7.2, §19.10, §19.26 simultaneously.

### 20.10 [HIGH/DRY] `IntegrationKind` and `ForgeType` hand-roll `as_i16`/`from_i16`
File: `core/src/ci/integration_lookup.rs:30-34, 46-69`. Two enums, each implements `as_i16` and `from_i16` by hand. The crate ecosystem already solved this:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, num_enum::IntoPrimitive, num_enum::TryFromPrimitive)]
#[repr(i16)]
pub enum IntegrationKind { Inbound = 0, Outbound = 1 }
```

Use `.into()` and `.try_into()`. 17 `from_i16`/`as_i16` call sites collapse to idiomatic Rust. Same for the `kind_to_str` / `forge_to_str` / `parse_kind` / `parse_forge` quartet in `endpoints/orgs/integrations.rs:99-135` — derive `serde::Serialize`/`Deserialize` with `#[serde(rename_all = "lowercase")]` and a `Display` impl.

### 20.11 [HIGH/DRY] 13 `i16` enum encoding sites total — `BuildStatus`, `EvaluationStatus`, `IntegrationKind`, `ForgeType`, `CacheSubscriptionMode`, `MessageLevel`, `Architecture`
The same hand-rolled `as_i16`/`from_i16` pattern repeats across multiple enums in `entity/`. One workspace-wide policy: every entity-stored enum derives `num_enum::TryFromPrimitive` + `IntoPrimitive`, period. Bonus: `serde` then refuses unknown integers at parse time instead of silently `Some(default)`.

### 20.12 [HIGH/DRY] 4 `*-name-available` endpoints
Already in §19.29; restating with measurement. Endpoints: `get_org_name_available`, `get_cache_name_available`, `get_project_name_available`, `post_check_username`. Each is ~25 LOC of boilerplate around one DB existence query. Replace with one generic helper or — better — return a structured `error_code: "name_taken"` from the create endpoint and delete all four.

### 20.13 [HIGH/DRY] `serde_json::json!` payload schema repeated 3× in webhook code
File: `core/src/ci/webhook.rs:132, 165, 226`. All three call sites produce:

```rust
serde_json::json!({
    "event": <event_str>,
    "data": { ... event-specific fields ... }
})
```

…with subtly different shapes for evaluation vs. build vs. test ping. Define the contract once:

```rust
#[derive(Serialize)]
struct WebhookPayload<'a, T: Serialize> {
    event: &'a str,
    data: T,
}

#[derive(Serialize)] struct EvaluationEvent { evaluation_id: Uuid, project_id: Option<Uuid>, repository: String, status: EvaluationStatus }
#[derive(Serialize)] struct BuildEvent { build_id: Uuid, evaluation_id: Uuid, derivation_path: Option<String>, status: BuildStatus }
```

…then `serde_json::to_string(&WebhookPayload { event, data: EvaluationEvent { ... } })`. Now the webhook contract is statically typed, the `unwrap_or_default()` in `post_webhook_test` (§11.8) goes away (`to_string` on a typed struct can't fail meaningfully), and we get OpenAPI / JSON-Schema generation for free if anyone ever needs it.

### 20.14 [HIGH/DRY] `get_X_by_name` vs `get_any_X_by_name` doubles every lookup
Files: `core/src/db/mod.rs` (and modules) — each resource has both:

- `get_organization_by_name(state, user_id, name)` — checks the user is a member.
- `get_any_organization_by_name(state, name)` — no membership check.

Same for caches, projects. Six lookup functions when there should be one with an enum:

```rust
async fn get_organization(state: &ServerState, name: &str, scope: AccessScope) -> Result<Option<MOrganization>>
where AccessScope = Public | MemberOf(Uuid) | Any
```

…or three functions explicitly named `get_public_organization_by_name`, `get_organization_member_by_name`, `get_organization_unchecked_by_name` so the access intent is in the call site. Anything but the current "one with a `user_id`, one without" convention which constantly invites picking the wrong one.

### 20.15 [HIGH/DRY] 36 places trim-and-validate strings inline
Search: 36 hits of `validate_display_name` / `.trim().to_string()` / `.trim()` in handlers. The pattern is always:

```rust
let trimmed = body.display_name.trim().to_string();
if let Err(e) = validate_display_name(&trimmed) { return Err(WebError::BadRequest(...)); }
active.display_name = Set(trimmed);
```

Lift to a `Trimmed`/`DisplayName` newtype with a `Deserialize` impl that does both. Then the handler signature uses `Json<MakeOrgRequest>` where `MakeOrgRequest.display_name: DisplayName` — validation happens at the deserialiser, not the handler. **Validate at the type boundary**, not 36 times after.

### 20.16 [HIGH/DRY] Three SSH-key-write-with-0600 sites
Files: `core/src/sources/ssh_key.rs:21-40`, `worker/src/executor/fetch.rs:181`, `core/src/nix/flake.rs:54`. Each writes a private key to a temp path and chmod's `0o600`. Each has its own RAII lifecycle (or doesn't — see §11.6).

```rust
struct EphemeralKeyFile { _file: tempfile::NamedTempFile }
impl EphemeralKeyFile {
    fn write(material: &[u8]) -> io::Result<Self> { ... }
    fn path(&self) -> &Path { self._file.path() }
}
```

`Drop` cleans up. 3 sites collapse to 3 lines.

### 20.17 [HIGH/DRY] 2 dispatch-loop tracing/error-handling copies
`outbound.rs::start_outbound_loop` and `dispatch.rs::start_dispatch_loops` invent the same "spawn, name, log started, loop tick, error-log on Err" wrapper. `worker_lifecycle.rs` likely does too. Solved by §20.9.

### 20.18 [HIGH/DRY] `BASE_ROLE_ADMIN_ID == ou.role || BASE_ROLE_WRITE_ID == ou.role` is a `Role` enum waiting to happen
Files: `endpoints/projects/mod.rs:173`, `endpoints/orgs/settings.rs:50-53`. The `Role` *concept* exists in the DB as the `role` table, but in code it's three `Uuid` constants (`BASE_ROLE_ADMIN_ID`, `BASE_ROLE_WRITE_ID`, `BASE_ROLE_VIEW_ID` — `core/src/types/consts.rs:30-32`). Lift to a real `Role` enum with `PartialOrd`:

```rust
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role { View = 0, Write = 1, Admin = 2 }

impl Role {
    pub fn from_uuid(u: Uuid) -> Option<Self> { ... }
    pub fn is_at_least(self, min: Self) -> bool { self >= min }
}
```

`if role.is_at_least(Role::Write) { ... }` — readable, type-safe, eliminates the disjunction chains.

### 20.19 [MEDIUM/DRY] 17 logging strings starting with "Failed to ..."
Search: 17 `tracing::warn!`/`tracing::error!` lines whose first argument starts with `"Failed to ..."`. This is reinventing `anyhow::Context`:

```rust
.context("Failed to read private key file")?  // -> creates the failure message
```

…and a single `tracing::error!(error = %e, "operation failed")` at the top-level handler logs everything. The current pattern *both* logs and propagates, so the same failure shows up in logs twice.

### 20.20 [MEDIUM/DRY] Nine `into_iter().map(<Type>::from).collect()` collapse to `IntoResponse` `From`
Sites: `webhooks.rs:117`, `orgs/integrations.rs:167`, `caches/management.rs::get`, `orgs/management.rs::get`. They all turn `Vec<MThing>` into `Vec<ThingResponse>`. With:

```rust
impl<T: From<U>, U> From<Vec<U>> for ResponseList<T> { ... }
```

…or just expose `MOrganization` as the response type directly when no field-shaping is needed. Half the *Response wrappers exist only to add `can_edit: bool` — a single optional field doesn't justify a parallel struct hierarchy.

### 20.21 [MEDIUM/DRY] Pagination boilerplate
Search: `paginator.num_items().await?` + `paginator.fetch_page(page - 1).await?` repeated in `orgs/management.rs:173-174`, `projects/management.rs::get`, etc. Lift to `paginated::<E>(query, params, db).await?` returning `Paginated<Vec<E::Model>>` directly.

### 20.22 [MEDIUM/DRY] `.expose()` on `SecretString`/`SecretBytes` is a leaky abstraction
The whole point of `SecretString` is to *not* be `Display`able and to be auditable. But every consumer immediately calls `.expose()`. Search: 30+ hits. The right shape is **scope-bounded** access:

```rust
impl SecretString {
    pub fn with_str<R>(&self, f: impl FnOnce(&str) -> R) -> R { f(&self.0) }
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R { f(self.0.as_bytes()) }
}
```

…and reserve `.expose()` for one or two FFI sites where a closure doesn't fit. Now grep for `.expose()` shows exactly the danger zones.

### 20.23 [MEDIUM/DRY] 4 sites strip `https://` / `http://` / `:` from `serve_url`
Already noted in §12.14. Restating with the count: `core/src/sources/cache_key.rs:57-60`, `core/src/sources/ssh_key.rs:150-156`, `endpoints/caches/helpers.rs:192-198`, `endpoints/caches/helpers.rs:269-275`. Same 3-line dance, four times. Lift to `core::sources::serve_url_hostname(serve_url) -> String`.

### 20.24 [MEDIUM/DRY] `WebError::*("...".to_string())` repeated 280 times
Search: 280 `WebError::` constructions in `endpoints/`. Many use the helper constructors (`WebError::not_found("Project")`) but a substantial fraction inline `WebError::BadRequest("custom string".to_string())`. Each unique English message is a *de facto* error code without a stable identifier. Combined with §12.10's `error_code` recommendation:

```rust
#[derive(thiserror::Error)]
pub enum WebError {
    #[error("name must contain only [a-z0-9-]")]
    #[code(400, "invalid_name")]
    InvalidName,
    ...
}
```

…the call site is `Err(WebError::InvalidName)` — 280 inline strings collapse to ~30 typed variants.

### 20.25 [MEDIUM/DRY] `if !cache.public { match maybe_user { ... } }` access pattern in 4 endpoints
`caches/management.rs::get_cache:239-244`, `endpoints/builds/mod.rs:111-118`, `endpoints/projects/mod.rs::get_project`, etc. The "public OR member" gate. Lift to:

```rust
fn ensure_visible(public: bool, maybe_user: &Option<MUser>, is_member: impl FnOnce() -> Future<bool>) -> WebResult<()>
```

…or fold into `ResourceCtx` (§20.2).

### 20.26 [MEDIUM/DRY] `validate_username`/`validate_password`/`validate_display_name`/`check_index_name`/`check_repository_url_is_ssh` all do "validator returns `Result<(), InputError>`"
File: `core/src/types/input.rs`. Five validators. Each handler then:

```rust
if let Err(e) = validate_X(&body.x) {
    return Err(WebError::BadRequest(format!("Invalid X: {}", e)));
}
```

There's already a `From<InputError> for WebError` in `error.rs:74-78`. Just `validate_X(&body.x)?` — no `format!`, no `.to_string()`, propagation does the work. Saves 3 lines per call, ~30 sites.

### 20.27 [MEDIUM/DRY] Repeated `tokio::sync::RwLock<HashMap<String, _>>` worker registry pattern
File: `proto/src/outbound.rs:32` (`Mutex<HashSet<String>>`), `scheduler/src/jobs.rs::JobTracker`, `scheduler/src/worker_pool.rs::WorkerPool`. Each is a Mutex/RwLock wrapping a HashMap of in-process state. They have similar idioms (insert-if-absent, remove on disconnect, iterate, count). A small `InProcessRegistry<K, V>` wrapper provides those 4 ops with consistent metrics. Currently each invents its own.

### 20.28 [MEDIUM/DRY] Direct-build "store files to disk" loop is a duplicated fragment of NAR pack
`endpoints/builds/direct.rs:97-114` writes user-uploaded files into a temp dir. This is conceptually "build a Nix flake from these inputs", which has at least two other in-tree implementations: the worker's flake fetcher and the test scaffolding. None of them share code. Lift to a `LocalFlakeStaging` type that owns the temp dir and returns a flake reference.

### 20.29 [MEDIUM/DRY] Email sender / webhook client / log storage are all `Arc<dyn Trait>` with the same shape
`Arc<dyn EmailSender>` (`storage/email.rs`), `Arc<dyn WebhookClient>` (`ci/webhook.rs`), `Arc<dyn LogStorage>` (`storage/log.rs`) all live on `ServerState` with the identical `Send + Sync + Debug + 'static` bound (see §19.23). Same builder pattern (`new()` returns `Result<Self>`). Lift to a `Service<I, O>` trait family or a `Provider<T>` trait once and have the three be marker traits.

### 20.30 [MEDIUM/DRY] `cli.serve_url.replace("https://", "")...replace("http://", "")` happens 6+ times
URL parsing should not be done with `replace`. Already partially noted in §12.14, §20.23. Add it to the `Cli` parse step:

```rust
pub struct AppConfig {
    pub serve_url: ServeUrl,    // a parsed url::Url
}
impl ServeUrl {
    pub fn host_port_string(&self) -> String { ... }   // for cache key naming
    pub fn as_url(&self) -> &Url { ... }
}
```

Parse once at startup, every consumer gets the right thing.

### 20.31 [MEDIUM/DRY] Two boolean toggle endpoints per resource: `post_X_active` + `delete_X_active`, `post_X_public` + `delete_X_public`
`caches/management.rs:358-388, 390-420`, `orgs/settings.rs:86-116`, `projects/management.rs:413-447`. Each pair is two near-identical handlers that differ only in `Set(true)` vs `Set(false)`. Replace with one `PUT /X/active` + `PUT /X/public` taking `{ "value": true|false }` — REST-shaped, half the code, half the routes.

### 20.32 [MEDIUM/DRY] `.unwrap_or_default()` over `String::new()` returning empty placeholders for missing OIDC claims
File: `oidc.rs:157-165`. Five `serde_json::Value::as_str().unwrap_or_default().to_string()` lines. Already flagged in §5.7. Use `serde_json::from_value::<UserInfoClaims>(user_data)?` once — strict mode with typed fields refuses unknown shapes and produces one error message instead of five empty strings.

### 20.33 [MEDIUM/DRY] Many endpoints call the same DB helper twice
e.g. `auth.rs::post_basic_register` does the email-send check at lines 86-93 and again at 112-122. The structure is:

```rust
let (a, b, c) = if condition { compute(); ... } else { ... };
... use a, b, c ...
if condition { send_with(b, ...) }
```

Two `if condition` branches with the same gate. Compute the email side-effects in one place and conditionally skip them.

### 20.34 [MEDIUM/DRY] 4 `Some(s) if !s.is_empty()` "trim and treat empty as None" patterns
Files: `orgs/integrations.rs:205-211, 214-220, 223-226, 228-234`. Same shape four times in one function:

```rust
let x = body.x.as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_owned);
```

…or in slight variations. Define `fn nonempty_trim(s: Option<String>) -> Option<String>` once.

### 20.35 [LOW/DRY] `tokio::time::interval(Duration::from_secs(N))` constants scattered
Periodic loop intervals: 15s (outbound), 30s (project poll), various others. Move to `core::types::consts` so an operator-facing config can override them later.

### 20.36 [LOW/DRY] `let _ = state.db.execute(...)` silent error sinks
~15 sites that fire-and-forget DB writes by binding to `_`. Each is its own metric/cleanup task. Combined with §7.2 + §20.9, all of these go through one named-spawn helper.

### 20.37 [LOW/DRY] `#[derive(Serialize, Deserialize, Debug)]` on every request DTO
Roughly every request type in `endpoints/` derives the same 3-trait quartet. Make a workspace-level `derive_request!` macro or a `derive_more::From, Display, ...` pattern. Saves visual noise.

### 20.38 [LOW/DRY] `Option<String>::as_deref().filter(|s| !s.is_empty()).is_none()`
Pattern repeats around endpoint URLs and tokens. Lift `fn is_empty_or_none(s: &Option<String>) -> bool`.

### 20.39 [LOW/DRY] Worker pool / scheduler test fixture duplication
`scheduler/src/dispatch_tests.rs`, `scheduler/src/scheduler_tests.rs`, `scheduler/src/handler_tests.rs` (2446 LOC!) all build similar `(state, scheduler, mock_db)` fixtures. The 2.4k file is the smoking gun for unfactored test scaffolding. Pull a `TestScheduler` builder into `test-support`.

### 20.40 [LOW/DRY] `#[cfg(test)] use super::*;` test glue
Every file with tests starts the same way. Group by module pattern and import once via a `tests/prelude.rs`.

### 20.41 [LOW/DRY] The two-pool pattern leaks: every handler picks `state.web_db` or `state.db` by hand
36 `find_by_id` sites all explicitly choose `&state.web_db`; some `state.db`. The choice is invisible to the type system (§19.5) and forces every author to remember the convention.

### 20.42 [LOW/DRY] `vec_to_hex` exists in `core::types::input` but its inverse is `hex::decode` (which is everywhere)
File: `core/src/types/input.rs`. `vec_to_hex(&bytes)` is `hex::encode(bytes)`. Just use the crate. Saves one helper.

### 20.43 [LOW/DRY] Migration files repeat the same `#[async_trait::async_trait] impl MigrationTrait for Migration { async fn up ... async fn down ... }` shell
74 migration files with the same scaffolding. `sea_orm_migration::prelude::Migration!()` macro could lift the boilerplate. Lower priority since each migration has unique content, but ~5 LOC × 74 = ~370 LOC saved, plus uniform error handling.

---

## 21. Cross-cutting refactors that retire whole categories at once

These are bigger than one finding — each absorbs a half-dozen DRY or quality items.

### 21.1 [HIGH] `ResourceCtx<R, Min: Role>` extractor
Folds in: §2.1, §2.2, §2.5, §2.6, §11.9, §12.1, §20.2, §20.18, §20.25.

```rust
let ctx: ResourceCtx<MProject> = ResourceCtx::require(...).await?;
ctx.org      // MOrganization, with role enforced
ctx.user     // MUser
ctx.role     // Role (>= Min)
ctx.resource // MProject
```

…with a generic `Min: Role` so the *type* `ResourceCtx<MProject, Admin>` proves the caller is at least admin. Anyone touching `ctx.resource` knows the ladder was checked.

### 21.2 [HIGH] `Json(BaseResponse {...})` removal + typed `WebError`
Folds in: §12.2, §12.10, §12.11, §20.5, §20.24. Sweeping change but mostly mechanical:

1. Replace every `Json(BaseResponse { error: false, message: t })` with `Json(t)`.
2. Replace every `WebError::BadRequest("...".to_string())` with a named variant.
3. Add `error_code: &'static str` to `WebError::IntoResponse`.

Frontend can switch on `error_code` once instead of pattern-matching English strings.

### 21.3 [HIGH] `BackgroundJobs` registry on `ServerState`
Folds in: §7.2, §7.4, §11.11, §12.18, §13.1, §13.4, §18.10, §19.10, §19.26, §20.9, §20.36.

One named-spawn API, one shutdown token, one tracing span per job, one metric per job, jitter for periodic loops. Every `tokio::spawn` in the codebase becomes `state.bg.spawn_named(...)`.

### 21.4 [HIGH] Strongly-typed config + DI seam
Folds in: §6.1, §12.12, §12.19, §12.20, §19.2, §19.3 (partially), §19.4, §20.4, §20.7, §20.30.

Split `Cli` into `AppConfig`, parse once, ID newtypes (`UserId`, `OrgId`, ...), drop entity_aliases.rs, move clients (`reqwest`, secret_loader) into config-built services. After this, every handler signature gets shorter and the test surface shrinks.

### 21.5 [MEDIUM] Typed PATCH derive macro (`#[derive(Patch)]`)
Folds in: §2.7 (uniqueness), §20.3, §20.15, §20.26, §20.32, §20.34. One project-local proc macro retires ~400 LOC of `if let Some` chains and centralises validation at the deserialiser layer.

### 21.6 [MEDIUM] `Crypter` service
Folds in: §6.2, §6.3, §6.5, §6.7, §11.5, §11.6, §11.7, §14.1.

```rust
pub struct Crypter { key: SecretBytes /* loaded once */ }
impl Crypter {
    pub fn seal(&self, plaintext: &[u8]) -> Vec<u8> { ... }
    pub fn open(&self, ciphertext: &[u8]) -> Result<Vec<u8>> { ... }
    pub fn ephemeral_keyfile(material: &[u8]) -> EphemeralKeyFile { ... }
}
```

Owns the `crypter` crate boundary, the credential dir, the `EphemeralKeyFile` RAII, and the SSH-plaintext-fallback removal in one place.

### 21.7 [MEDIUM] DB extension trait `LoadOrNotFound`, `paginated()`, `with_txn()`
Folds in: §19.1, §20.6, §20.21. Three methods on a `ServerState`-extension trait that absorb every "load or 404 / paginate / transaction" caller.

### 21.8 [LOW] `entity::EnumExt` workspace-wide derives
Folds in: §12.3, §19.6, §20.10, §20.11. Every entity-stored enum derives:

```rust
#[derive(num_enum::IntoPrimitive, num_enum::TryFromPrimitive, EnumIter, Display)]
#[repr(i16)]
```

…and `EvaluationStatus::is_active()`/`BuildStatus::is_terminal()` impls live next to the enum, not in 7 callers.

---

## 22. Logging — levels, structure, missing audit trail

**Top-line numbers** (non-test code, grepped): `trace=24`, `debug=140`, `info=155`, `warn=254`, `error=123`. Total **559 tracing calls**, distributed unevenly. The `warn` skew (twice as many as `error`) is the smoking gun: `warn!` is the catch-all when the author wasn't sure.

### 22.1 [HIGH] Logger is initialised AFTER `init_state` — startup logs are dropped silently
File: `backend/src/main.rs:64-71`:

```rust
async fn run() -> std::io::Result<()> {
    let cli = Cli::parse();
    let state = init_state(cli).await;            // ← any tracing! call here goes nowhere
    init_logging(&state.cli);                      // ← subscriber installed here
    info!(version = ..., "Starting Gradient server");
    ...
}
```

`init_state` runs schema migrations, applies the state file, builds the DB pool, generates SSH/cache keys, decrypts secrets — i.e. **the entire failure-prone bootstrap path** runs with no subscriber. Tracing's macro semantics drop the events on the floor. If migration fails, the operator sees nothing.

Fix: install the subscriber from CLI args alone (or via env var) *before* `init_state`. CLI overrides via env var still work (`RUST_LOG`).

### 22.2 [HIGH] `eprintln!` + `process::exit(1)` in `load_secret` bypasses the logger entirely
File: `core/src/types/input.rs:219-269`. Four `eprintln!("Failed to read secret file ...")` paths followed by `std::process::exit(1)`. None of these reach the structured logger; they go straight to fd2. Two effects:

1. JSON-log aggregators (Loki, Datadog) miss the event entirely.
2. Combined with §6.1 (re-read on every request), one transient FS error during a JWT decode kills the server with **no trace event** to correlate against.

Fix: load secrets once at startup, propagate `Err`, exit through a single instrumented path.

Same pattern in `worker/src/main.rs:161` (`eprintln!("invalid log directive ...")`) and `worker/src/nix/eval_worker.rs:103, 114` — eval-worker subprocess crashes silently from the orchestrator's POV.

### 22.3 [HIGH] Printf-style logging dominates 8:1 over structured fields
Search:
- 49 hits of `tracing::error!("…{}…", e)` — printf style.
- 6 hits of `tracing::error!(error = %e, "…")` — structured.

The codebase is mid-migration to structured: `endpoints/projects/evaluations.rs:541` already does it right (`output_path = %output_root, %rel, error = %e`); `endpoints/auth.rs:120` doesn't. Pure DRY pain — log aggregators can't filter by field on the printf calls.

Concrete examples to convert:

```rust
// Current
tracing::warn!("Failed to send verification email: {}", e);
tracing::error!("Failed to generate cache key: {}", e);
tracing::error!("Organization {} not found", organization_id);

// After
tracing::warn!(error = %e, %email, "Failed to send verification email");
tracing::error!(error = %e, %cache_id, "Failed to generate cache key");
tracing::warn!(%organization_id, "Organization not found for evaluation");
```

The first two should also include the relevant resource ID; the third demoted to `warn!` (see §22.5).

Lint candidate: `clippy::print_with_newline` exists; a project-local clippy-lint that flags `tracing::*!("…{}…")` would prevent regression.

### 22.4 [HIGH] No security-event audit logging
Searched the auth surface (`endpoints/auth.rs`, `authorization/middleware.rs`, `authorization/jwt.rs`, `authorization/oidc.rs`). Result: **zero `info!` events for any authentication or authorization decision**:

- `post_basic_login` — no log on success (no audit trail), no log on failure (no intrusion-detection signal).
- `decode_jwt` — invalid token rejection silent (no replay-attack signal).
- `post_logout` — silent.
- OIDC callback — no log of which user authenticated.
- `require_superuser` — no log when a superuser action authorizes (e.g. GitHub App manifest, `delete_user`).
- `delete_organization` / `delete_cache` / `delete_keys` — no log when destructive ops succeed.
- `post_organization_users` / `patch_organization_users` / `delete_organization_users` — no log when a role changes (the §2.1 RBAC bug would be invisible even if exploited).
- Email-verification flips — no log.

The two log lines that DO exist in this surface are `warn!("GitHub App webhook: invalid signature")` and the forge-webhook equivalent (`forge_hooks/mod.rs:69, 166`). Both are good; everything else is silent.

A baseline ten-line audit set:

```rust
info!(user_id = %user.id, %username, ip = %client_ip, "auth.login.success");
warn!(%username, ip = %client_ip, reason = "invalid_password", "auth.login.failure");
info!(user_id = %user.id, ip = %client_ip, "auth.logout");
warn!(reason = %e, ip = %client_ip, "auth.token.invalid");
info!(actor_id = %actor.id, target_id = %target.id, role = ?new_role, "org.member.role_changed");
info!(actor_id = %actor.id, %org_name, "org.deleted");
info!(actor_id = %actor.id, %cache_name, "cache.deleted");
info!(actor_id = %user.id, key_id = %key.id, "api_key.created");
info!(actor_id = %user.id, key_id = %key.id, "api_key.deleted");
info!(actor_id = %actor.id, target_id = %target.id, "user.deleted");
```

Pair with the `audit_log` table in §15 to make the events queryable.

### 22.5 [HIGH] `error!` is overused for "user gave bad input" / "row not found"
Files: `endpoints/evals/mod.rs:55, 73, 84`, `endpoints/builds/mod.rs:53, 65, 80, 89`. All are:

```rust
tracing::error!("Organization {} not found", organization_id);
tracing::error!("Project {} not found for evaluation {}", ..., ...);
tracing::error!("DirectBuild not found for evaluation {}", evaluation_id);
```

These are reachable when:
- A request references a deleted ID (user error → `warn!` at most, often `debug!`).
- A FK is broken (server bug → genuine `error!`).

Both cases are conflated under one log level. Pages people. Solution: add a domain — `error!(target: "data_inconsistency", ...)` for the FK case, `warn!(target: "user_input", ...)` for the request case. Or split into a typed `DataInconsistency` error variant which always logs `error!`, and a `not_found` variant which logs `debug!`.

### 22.6 [HIGH] `WARN` is the catch-all (254 sites) — drowns real warnings
The 2:1 `warn:error` ratio is wrong. A real prod service has more `error!` than `warn!` because errors are paged. Audit a sample:

| File:line | Current | Should be |
|----------|---------|-----------|
| `auth.rs:120` "Failed to send verification email" | `warn!` | `error!` (operator must know SMTP is down) |
| `forge_hooks/mod.rs:53` "GitHub App webhook received but not fully configured" | `warn!` | `info!` once at startup (currently logs on every webhook delivery) |
| `forge_hooks/mod.rs:122` "Unknown forge path segment" | `warn!` | `debug!` (caller error, not server problem) |
| `caches/nar.rs::record_nar_traffic` "Failed to update cache metric" | `warn!` | `error!` (data loss; pages oncall) |
| `outbound.rs::connect_to_registered_workers` "outbound connection timed out (10s)" | `error!` | `warn!` (transient network; only error after N consecutive) |
| `proto/handler/auth.rs:108` "failed to look up registered peers" | `warn!` | `error!` (auth fail-closed; data plane impacted) |

The pattern is: anything that *might* be transient should be `warn!`; anything that means data loss, security degradation, or operator action required is `error!`. The current code uses `warn!` for both.

### 22.7 [HIGH] No request-id / trace-id propagation
File: `web/src/lib.rs:51-71`. `TraceLayer::new_for_http()` is configured with `on_request` / `on_response` callbacks but no `make_span_with` or `MakeRequestId`. Symptoms:

- A request that hits 4 DB queries + 1 webhook fire produces 5 separate log lines with no link between them.
- Async tasks spawned from a handler (`tokio::spawn` in `caches/management.rs::delete_cache`) lose all request context — the cleanup logs are orphaned.
- Federated proto messages don't carry a correlation ID.

Add `tower_http::request_id::SetRequestIdLayer + PropagateRequestIdLayer`, log the request id on every span, propagate via headers (`X-Request-Id`) and protocol messages.

### 22.8 [HIGH] Worker subprocess `eprintln!` lines never reach the parent's structured log
File: `worker/src/nix/eval_worker.rs:103, 114`. Eval-worker is a subprocess driven by line-delimited JSON over stdin/stdout. Its `eprintln!` writes to stderr, which the parent might inherit OR might capture per-line and re-log — but currently doesn't. So when an eval-worker dies on `NixEvaluator init failed`, the supervisor sees the process exit with no diagnostics in the structured log.

Fix: capture child's stderr line-by-line, re-emit as parent's `error!(target: "eval_worker", line = %line)`.

### 22.9 [MEDIUM] `#[instrument]` used in only ~9 places out of 559 tracing calls
Search: 9 hits across the workspace. The codebase uses prefix-string-in-message (`"poll_projects:"`) instead of structured spans. Mass-instrumenting handlers is cheap:

```rust
#[instrument(skip(state), fields(user_id = %user.id, %organization))]
pub async fn delete_organization(state: ..., Extension(user): ..., Path(organization): ...) -> ... { ... }
```

Now every log line inside (DB error, cleanup spawn, etc.) automatically inherits `user_id` and `organization` fields. Combined with §22.7, this is the single highest-leverage observability change.

### 22.10 [MEDIUM] No JSON formatter configured
File: `backend/src/main.rs:48-52`:

```rust
tracing_subscriber::registry()
    .with(fmt::layer().with_target(true).with_thread_ids(true))
    .with(env_filter)
    .init();
```

`fmt::layer()` defaults to human-readable. For prod (Loki/ELK/Datadog), JSON output is mandatory. Make it a CLI flag:

```rust
#[arg(long, env = "GRADIENT_LOG_FORMAT", default_value = "human")]
pub log_format: LogFormat,  // enum { Human, Json, Compact }
```

…and switch to `fmt::layer().json()` when `LogFormat::Json`.

### 22.11 [MEDIUM] Per-crate log-level config is by Cargo crate name, not by domain
File: `backend/src/main.rs:14-32`. CLI exposes `--builder-log-level`, `--cache-log-level`, `--web-log-level`, `--proto-log-level`. These map to crate names. But operators want to tune by *concern*: "elevate auth events to info!" or "silence the dispatch loop". Two missing dimensions:

- A `gradient::audit` filter target (independent of crate) that auth/RBAC log lines emit under, so operators can do `RUST_LOG=warn,gradient::audit=info`.
- A `gradient::dispatch` target for the eval/build poll loops (which are noisy).

Set the targets via `tracing::info!(target: "gradient::audit", ...)` at the audit sites.

### 22.12 [MEDIUM] `DEBUG` logs in TraceLayer for request entry/exit
File: `web/src/lib.rs:54-58`:

```rust
.on_request(|request, _span| {
    tracing::debug!("started {} {}", request.method(), request.uri().path())
})
```

Access logs at `debug` level means prod (running at `info`) gets no record of which requests came in. Move to `info!` with structured fields (`method`, `path`, `status`, `latency_ms`, `user_id`, `request_id`). This is *the* access log — every prod web service has one at info.

Conversely, `on_body_chunk` at `debug!` is too verbose for any sane production level — should be `trace!`.

### 22.13 [MEDIUM] 35 `let _ = …` silent error sinks (already §7.2)
Restating with concrete examples:

- `endpoints/caches/nar.rs:122-138` — `let _ = state.db.execute(...).await;` (cache_derivation last_fetched_at update; data drift if it fails).
- `worker_lifecycle.rs` — broadcast channel send (probably OK).
- Every fire-and-forget spawn (§20.9).

The `BackgroundJobs` registry (§21.3) eliminates this category by routing through `spawn_named` which always logs failures.

### 22.14 [MEDIUM] 72 `unwrap_or_default()` sites swallow `Result`s
Different from `let _`: `unwrap_or_default()` actively converts `Err` into a default value, which the caller then can't distinguish from "really empty". Examples:

- `endpoints/orgs/management.rs:182, 188` — `.unwrap_or_default()` on org-membership lookup → "user has no orgs" silently masks DB errors.
- `endpoints/builds/direct.rs:182, 188` — `.unwrap_or_default()` on direct-builds list → "no recent builds" same.
- `webhooks.rs:266` — `.unwrap_or_default()` on JSON serialisation → empty body delivered (§11.8).

Each is a candidate for `?` propagation or an explicit `match { Err(e) => warn!(...); ... }`.

### 22.15 [LOW] `info!` on every loop tick is missing
File: `proto/src/outbound.rs:34`, `scheduler/src/dispatch.rs:54`. Each loop logs `"started"` once at boot, then nothing until errors. A periodic `debug!(jobs_processed = N, "dispatch loop tick")` lets operators verify liveness without enabling trace-level. Combined with metrics, this is the heartbeat.

### 22.16 [LOW] `tracing::error!` for user-facing 4xx response building
Files: `endpoints/auth.rs:247, 279`. `Response::builder().body(...)` is logged as `error!` then converted to a 500. In axum, `Body::empty()` cannot fail. Either:
- Drop the log entirely and rely on `?`.
- If the failure mode is genuinely possible, log at `warn!` and include the offending header/path.

### 22.17 [LOW] Logged secrets — quick audit
A search for `tracing::*!.*\(secret|password|token|key\)` found no overt leaks. Two near-misses to keep an eye on:
- `error.rs:103` — `tracing::error!("Database error: {}", err);` — `DbErr::Display` includes the failed *query parameters* in some sea-orm versions. With user-supplied JWTs as query params elsewhere, a bad DB error could log a JWT.
- `error.rs:117` — `tracing::error!("Internal error: {}", err);` — `anyhow::Error::Display` walks the chain. Any wrapped reqwest error that came from an OIDC token request might dump the token.

Mitigation: a `Sanitize` layer that scrubs known patterns (`Bearer …`, `GRAD…`, `-----BEGIN …`, `Authorization: …`) from log lines before they reach the writer. `tracing-subscriber` supports custom layers for this; the team should add one.

### 22.18 [LOW] Dynamic-size events at info level
Several `info!` in `core/src/state/provisioning.rs:237, 256, 273-277` log "Updated managed organization: {name}" — fine, but on a state file with 100 orgs, that's 100 lines per reconcile. Consider summarising: `info!(updated = 100, created = 5, "applied organizations")` and demote per-row to `debug!`.

---

## 23. TODOs, missing implementations, and silent half-features

### 23.1 [HIGH] Explicit TODOs in handler code
Eight TODO/FIXME hits in non-test code:

| Site | TODO | Risk |
|------|------|------|
| `web/src/endpoints/user.rs:109` | "Make sure to delete all related data and that cascade is working" | High — `DELETE /user` may leak rows or fail silently (§2.5). |
| `web/src/endpoints/commits.rs:25` | "Check if user has access to the commit" | **High — IDOR**: anyone with a commit UUID can fetch any commit. |
| `web/src/endpoints/forge_hooks/trigger.rs:119` | "save canonicalised URLs in the DB so the lookup can be done with eq() and an index instead of LIKE" | Medium — current `LIKE` matching on every webhook is unindexed. |
| `web/src/endpoints/evals/log.rs:65` | "Chunkify past log" | Medium — log fetch buffers the entire log in memory (§3.3-shape OOM). |
| `web/src/endpoints/caches/management.rs:111` | "Implement pagination" in `GET /caches` | Medium — unbounded result set; once a user has 10k caches the endpoint hangs. |
| `core/src/ci/integration_lookup.rs:174` | "implement GitLabReporter" | Low (silent feature gap) — see §23.4. |
| `scheduler/src/handler_tests.rs:2218` | test scaffolding | Test only. |
| `forge_hooks/mod.rs:173` | `unreachable!("checked above")` | Low — runtime panic if the GitHub branch is reached due to a refactor bug. |

### 23.2 [HIGH] `commits.rs` — endpoint exists with no authorization
File: `endpoints/commits.rs:25`. The TODO says "Check if user has access". Without it, `GET /commits/{commit}` returns commit metadata (message, author name, hash) for any commit in the system. Cross-tenant data leak: a user in org A can read commit info from org B's private projects.

This belongs in the security top-line table — strictly speaking it's the same shape as §2.4 (`user::get_search`) — globally readable resource.

### 23.3 [HIGH] `delete_user` cascade is unaudited (§2.5 + 23.1.1)
`AUser.delete()` relies on FK cascades. With the FK-cascade gaps documented in §19.12 (some FKs have explicit cascade, some don't), a `DELETE /user` request can:
- Succeed → leaving orphan rows (e.g. `direct_build.created_by` if not `ON DELETE CASCADE`).
- Fail with 500 → cleanup half-done, user stuck unable to retry.

Until both the cascade audit (§19.12) and the password re-auth (§2.5) land, document the endpoint as alpha.

### 23.4 [HIGH] Silent feature stubs (fail closed but invisibly)

| Feature | Where | Symptom |
|---------|-------|---------|
| GitLab outbound CI status reporting | `core/src/ci/integration_lookup.rs:173-176` returns `Arc::new(NoopCiReporter)` for `ForgeType::GitLab` | Operator configures a GitLab integration, no `commit-status` is ever posted. No log warns about the gap. |
| API key revocation | DB has no `revoked_at` column; UI offers "delete" only | Compromised API keys must be deleted, which deletes the row's audit trail too. |
| API key scopes | DB has no `scopes` column; every key is "full session JWT-equivalent" | §1.6. Mentioned in features list but not implemented. |
| JWT revocation on logout | `post_logout` clears cookie, JWT remains valid until `exp` | §1.9. |
| Audit log | Table doesn't exist | §13.6 / §15. The §22.4 audit-log lines have nowhere to write. |
| Webhook delivery history | Table doesn't exist | §15. Operator can't see which webhooks failed. |
| Per-org quotas | No quota table | §18.13. No defence against tenant abuse. |
| Worker token revocation | DB has hashed token but no `revoked_at` | §11.2. Compromised worker = delete-and-recreate, breaking history. |
| HTTP/3 (QUIC) | `cli.quic` is exposed via `/api/v1/config` (`endpoints/mod.rs:132`) but the server itself does no HTTP/3 termination | Fine — comment at `mod.rs:113-117` explicitly says "termination is handled by the reverse proxy". Only a "feature" half-spec'd at the API surface. |
| Federation auth across servers | `federate_proto` flag exists; in-protocol auth shape exists; no end-to-end test or doc in `docs/` covers the trust model | Documentation gap; could be an implementation gap. |

The pattern is consistent: features are added as flags but their data-model dependencies aren't. **Recommend a "feature registry" review**: every flag in `Cli` should be cross-checked against (a) DB schema support, (b) operator-facing docs, (c) integration test, (d) frontend visibility.

### 23.5 [HIGH] `cli.max_proto_connections` is declared but unused
File: `core/src/types/mod.rs:213`:

```rust
#[arg(long, env = "GRADIENT_MAX_PROTO_CONNECTIONS", default_value = "256")]
pub max_proto_connections: usize,
```

Search for consumers: zero. The flag is documented to "Maximum number of simultaneous proto WebSocket connections" — but nothing in `proto/src/handler/` ever consults it. Workers can keep opening connections forever; the only limit is the OS file-descriptor ceiling. Combined with §18.9, this is a real DoS vector with a flag that pretends to defend it.

### 23.6 [HIGH] `// TODO: Implement pagination` on `GET /caches`
File: `caches/management.rs:107-149`. Returns `Vec<MCache>` of all caches the user can see. With 10k+ caches per user (plausible for a busy org), this is unbounded. The struct `Paginated<T>` already exists (§19.32) and is used for orgs/projects — caches were skipped. Five-line fix.

### 23.7 [MEDIUM] `report_errors` flag has consumers but no docs
File: `backend/src/main.rs:82`, `cache/src/cacher/mod.rs:31, 77`. Three sites use `state.cli.report_errors` to install a `_guard` — likely Sentry/glitchtip. The flag isn't documented in `docs/usage/` nor in `nix/modules/gradient.nix`. Operator surface is invisible.

### 23.8 [MEDIUM] `evals/log.rs::Chunkify past log` TODO
File: `endpoints/evals/log.rs:65`. The log endpoint streams the live log, but for completed builds it dumps the entire stored log into memory before responding. Any large build (a kernel rebuild, a multi-GB rust workspace) OOM's the server. Pair with §3.3.

### 23.9 [MEDIUM] `forge_hooks/trigger.rs::TODO save canonicalised URLs`
File: `endpoints/forge_hooks/trigger.rs:119`. Webhook routing matches the pushed repo URL against `Project.repository` with case-insensitive substring matching. Two issues:

1. Performance: every webhook scans every project of the org with `LIKE`, which can't use a btree index.
2. Correctness: `https://github.com/foo/bar` matches `https://github.com/foo/barbarbar` if the `LIKE` is wrong. (Probably is — needs verification.)

Canonicalise on project create/update; index. Standard fix.

### 23.10 [MEDIUM] `unreachable!()` in handler code is a runtime panic waiting
File: `endpoints/forge_hooks/mod.rs:173` — `ForgeType::GitHub => unreachable!("checked above")`. The check happens 50 lines earlier; a refactor that moves the early-out reaches this branch and the request panics → axum returns 500 from the panic catcher, but the team will see "thread panicked" in logs. Replace with an exhaustive `match` and a `WebError::Internal(...)` variant.

### 23.11 [MEDIUM] Direct-build `temp_dir` cleanup never happens (§5.3 + here)
The `tokio::fs::create_dir_all` succeeds, files are written, then nothing — no `Drop` guard, no scheduled cleanup. Only the worker (when it consumes the job) deletes the dir. If the worker never claims the job (queue backed up, worker offline, evaluation aborted), the dir lives forever. Implicit TODO — the cleanup logic is missing entirely.

### 23.12 [MEDIUM] `keep_evaluations: 0` default disables GC
File: `core/src/types/mod.rs:129-130` — default for `cli.keep_evaluations` is 0 = "keep forever". `scheduler/src/eval.rs:366-369` enforces it per-project, but the *server-wide* default never deletes anything. After a year of operation, the `evaluation` table grows unboundedly. Default should probably be a sane value (50?). Document explicitly.

### 23.13 [MEDIUM] `nar_ttl_hours: 0` default disables NAR TTL
Same shape: `core/src/types/mod.rs:147-148` defaults the NAR cleanup TTL to 0 (disabled). `cache/src/cacher/cleanup.rs:40-54` correctly skips when `ttl_hours == 0`. Operators must opt in. For prod with finite disk, this should default to something — 30 days, say — with explicit documentation.

### 23.14 [MEDIUM] `keep_orphan_derivations_hours: 24` is the only GC default with a real value
Same group; this one defaults to 24h. Inconsistent defaults across three GC knobs that should probably use a unified `GcConfig` with documented defaults (§19.2).

### 23.15 [LOW] `// TODO: Add a RecordingStatusReporter or similar to capture the source field.`
File: `scheduler/src/handler_tests.rs:2218`. Test scaffolding gap — observed source fields aren't asserted, so a regression that drops the `source` field in CI status updates passes the suite. Noted for the testing-coverage backlog.

### 23.16 [LOW] `unimplemented!()` / `todo!()` macros: zero hits
Good — no panicking placeholders ship in production paths. The only `unreachable!` is the one in §23.10.

### 23.17 [LOW] No deprecation policy for `Cli` flags
Several flags are clearly historical (`use_tls` is essentially "do we set the cookie Secure flag"; `quic` is advisory metadata). When fields are removed or renamed, env-var migration path? Currently silent. Stand a `--no-warn-deprecated` toggle and emit `warn!("--use-tls is deprecated; set GRADIENT_USE_TLS")` for one release.

### 23.18 [LOW] `oidc_required` + `enable_registration` interaction is documented in code only
File: `endpoints/auth.rs:53` — `if !state.cli.enable_registration || state.cli.oidc_required { ... }`. The fact that `oidc_required` overrides `enable_registration` is invisible to the operator until they hit the registration endpoint. Surface in `/api/v1/config` (already partly there) and in operator docs.

### 23.19 [LOW] Missing implementations indicated by half-set `Optional` fields
Search: `endpoint_url: Option<String>` (`integration` table), `secret: Option<String>` (`integration`), `last_evaluation: Option<Uuid>` (`project`), `keep_evaluations: i32` (`project`, with 0 as off). The mix of `Option` and "0 means none" sentinels makes data-model intent inconsistent. Migrate "0 means none" sentinels to nullable columns or a typed `Disabled | Enabled(N)` enum stored as JSON.

### 23.20 [LOW] `last_check_at` on `project` exists but is never read
File: `entity/src/project.rs` (presumed; column exists per `provisioning.rs:335`). Set on project creation, but the dispatcher uses `last_evaluation.created_at` for "is it time to poll" (`scheduler/src/dispatch.rs::poll_projects_for_evaluations`). The column is dead schema. Either start using it (cheaper than the JOIN) or drop it.

### 23.21 [LOW] `is_artefacts_*` half-flags
Per the project memory note: `derivation_output` had a `has_artefacts bool` column added then dropped (`m20260330` → `m20260421_000004`). The API still exposes a `has_artefacts: bool` derived from `build_product` row count (compat shim). Fine — the API surface is stable — but the migration history shows churn.

### 23.22 [LOW] Per-project `force_evaluation` flag with no UI affordance
Field exists on `project` table (`m20241107_135941_create_table_project.rs`-era), is read by `scheduler/src/jobs.rs` and `core/src/sources/git.rs:104`. Could be set via `PATCH /projects/...` — but the `MakeProjectRequest`/`PatchProjectRequest` structs in `endpoints/projects/management.rs:32-49` don't expose it. So the field is settable only via state-file provisioning. Noted as half-exposed; might be intentional.

### 23.23 [LOW] Documentation gaps
Per the project's own CLAUDE.md: "don't forget to update docs/gradient-api.yaml when changes on api are made". With ~120 routes and ~30 PATCH/POST shapes, the OpenAPI YAML is almost certainly drifted. A CI check (`schemathesis run docs/gradient-api.yaml --base-url=http://...`) catches drift mechanically.

---

## 24. User-supplied items — verified against the codebase

Each user-flagged TODO was checked against the actual source. All eight reproduce; details and exact file/line evidence below.

### 24.1 [HIGH] Log OIDC errors in journal at `error!` — confirmed missing
**Files:** `backend/web/src/authorization/oidc.rs` (entire file), `backend/web/src/endpoints/auth.rs:188-285`, `backend/web/src/error.rs:94-122`.

`oidc.rs` contains **zero** `tracing::*` calls. Every error inside `oidc_login_create` / `oidc_login_verify` / `create_or_update_user` is propagated up via `anyhow::Context("...")` and surfaces in `auth.rs` as:

```rust
let user: MUser = oidc_login_verify(state.clone(), code.to_string())
    .await
    .map_err(|e| WebError::InternalServerError(e.to_string()))?;        // auth.rs:194-196
```

The crucial detail: `WebError::InternalServerError(msg)` maps to `(500, msg)` in `error.rs:101` *without* logging — only `WebError::Internal(anyhow::Error)` logs (`error.rs:117`). So the OIDC failure ends up:

- written to the **HTTP response body** (potential info disclosure of upstream IdP error strings, OIDC discovery URLs, decode-failure hints);
- **never written to journald / structured logs**.

An operator debugging an OIDC outage gets nothing from `journalctl -u gradient`. They have to capture a 5xx response body from their reverse proxy, which most don't log.

**Fix:** wrap each `oidc_*` site in `instrument`-spans, `tracing::error!(error = %e, "OIDC X failed")` before the error becomes a `WebError`. Pair with §22.4 (security event audit logging) so successful OIDC login also emits an `info!`. Also: route through `WebError::Internal(e)` instead of `InternalServerError(e.to_string())` so the existing log path fires.

This is also a security-grade gap: OIDC failures are exactly where incident response wants telemetry (compromised IdP, MITM, replay attempts). Tied to the larger §1.1–1.3 / §22.4 audit-trail story.

### 24.2 [HIGH] Organization members missing from state configuration — confirmed missing
**Files:** `backend/core/src/state/mod.rs:31-47` (`StateOrganization`), `backend/core/src/state/provisioning.rs:181-282` (`apply_organizations`).

`StateOrganization` has **no `members` field**:

```rust
pub struct StateOrganization {
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub private_key_file: String,
    pub public: bool,
    pub github_installation_id: Option<i64>,
    pub created_by: String,
}
```

`apply_organizations` (lines 259-278) only adds `created_by` as an admin membership row, then stops. There is no path to declare further members or roles in the state file — yet `StateProject`/`StateCache`/`StateApiKey` all reference the org by name and assume membership exists. Operators have to bootstrap orgs via state and then add members through the API, which defeats the "managed entirely by configuration" promise.

**Fix:** extend `StateOrganization`:

```rust
pub struct StateOrganization {
    ...,
    #[serde(default)]
    pub members: Vec<StateOrgMember>,
}

pub struct StateOrgMember {
    pub user: String,           // username
    pub role: String,           // "admin" | "write" | "view"
}
```

…then `apply_organizations` reconciles the `organization_user` table just like `apply_project_integration_links` reconciles its joins. Tied to §2.1 (RBAC fix) — once `Role` is a real enum (§20.18), serde does the parsing for free.

Operator-side: `nix/modules/gradient-state.nix` (assumed location) needs the corresponding option, and the validation in `state/mod.rs::validate()` (lines 213-380) needs to refuse roles outside the closed set.

### 24.3 [HIGH] `Child::kill()` plumbing through `WorkerPoolResolver` — confirmed missing
**Files:** `backend/worker/src/worker_pool/resolver.rs:34-44`, `backend/worker/src/worker_pool/pool.rs:38-90, 178-360`.

`WorkerPoolResolver` exposes only `new(workers, max_eval_per_worker)` and the `DerivationResolver` trait methods. There is **no `shutdown()` method**, and the inner `EvalWorkerPool` likewise lacks one. Today's lifecycle:

- Each subprocess is spawned with `.kill_on_drop(true)` (`pool.rs:46`).
- An individual `EvalWorker` has a graceful path: `drop(self.stdin)` then a 5-second wait for clean exit (`pool.rs:178-208`), falling back to SIGKILL via `kill_on_drop`.
- That graceful path runs only when a single worker is poisoned/dropped — *not* when the server orchestrator wants to shut down the whole pool.

Consequence: on `SIGTERM` to the gradient binary, the eval workers are SIGKILL'd as the `Arc<WorkerPoolResolver>` is dropped (whenever that is — see §19.26's "no shutdown coordination"). Workers in mid-evaluation lose their Boehm-GC state, possibly leak temp files / GC roots, and the parent has no `JoinSet` to await orderly drain.

**Fix:** add a `shutdown()` method on `WorkerPoolResolver` that asks `EvalWorkerPool` to:

1. Stop accepting new acquires (close the semaphore).
2. For each pooled worker, send `Shutdown` over stdin and `Child::wait` with a deadline.
3. SIGKILL any holdouts after timeout.

Wire it into the `BackgroundJobs` registry from §21.3 / §19.26 so the existing `CancellationToken` triggers it. The implementation is small; the missing piece is the API surface.

### 24.4 [MEDIUM] `env."preferLocalBuild"` not stored on the derivation row — confirmed
**Files:** `backend/entity/src/derivation.rs:14-21` (schema), `backend/core/src/db/derivation.rs:31-47, 232-…` (`parse_drv` produces `environment: HashMap<String, String>`), `backend/worker/src/executor/build.rs:518` (only consumer is the worker).

The schema has:

```rust
pub struct Model {
    pub id: Uuid,
    pub organization: Uuid,
    pub derivation_path: String,
    pub architecture: super::server::Architecture,
    pub created_at: NaiveDateTime,
}
```

— five columns. `preferLocalBuild`, `allowSubstitutes`, `allowedReferences`, etc. all live in the .drv's `env` map, which is parsed into `Derivation::environment` and then dropped on the floor by the eval-side persistence path. The worker reads `self.drv.environment` to drive the daemon, but the **scheduler** can't make policy decisions ("small derivation, prefer local builder over expensive remote").

**Fix:** add a column or two in a migration:

```sql
ALTER TABLE derivation
    ADD COLUMN prefer_local_build BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN allow_substitutes BOOLEAN NOT NULL DEFAULT true;
```

…populate during eval-result ingestion (where `Derivation::environment` is available), and consume in `scheduler/src/policy.rs` to bias short / `preferLocalBuild=1` builds away from remote workers. Also surface as a hint in `BuildJob` messages so workers can short-circuit substitution probes.

(Also worth storing: `requiredSystemFeatures` is parsed but only used for *gating*; storing it explicitly enables better worker-affinity scoring instead of re-parsing each time.)

### 24.5 [HIGH] Evaluations stuck in `Queued` when no worker is available — confirmed
**Files:** `backend/scheduler/src/dispatch.rs:187` (initial `Queued` filter), `backend/scheduler/src/build.rs:252-310` (`reconcile_waiting_state`), `backend/scheduler/src/build.rs:259-260` (only operates on `Building` / `Waiting`).

The current state machine:

- Evaluations are inserted with `status = Queued` (e.g. `core/src/ci/trigger.rs:84`, `endpoints/builds/direct.rs:137`).
- `dispatch.rs:187` polls `Status.eq(Queued)` and enqueues `FlakeJob`s.
- After eval-step completion, builds are inserted and the eval flips to `Building`.
- `reconcile_waiting_state` (`build.rs:252`) **only considers** evaluations whose status is `Building` *or* `Waiting`:

```rust
let evals = EEvaluation::find()
    .filter(CEvaluation::Status.is_in(vec![EvaluationStatus::Building, EvaluationStatus::Waiting]))
    .all(&self.state.db).await?;
```

So an evaluation that:
- Runs evaluation, produces builds.
- The builds fail to dispatch (no worker advertises matching `architecture` / `system_features`).
- Sits there.

…is not picked up by the reconciler — and it stays at `Queued` (or `Building` if it briefly transitioned). The user's distinction is the right one:

- **`Queued`** should mean *"waiting to be evaluated/dispatched"* — eval-side queueing.
- **`Waiting`** should mean *"all builds queued, no compatible worker available"* — build-side gate.

Today the codebase mostly treats them as synonyms. A user opens the dashboard and sees `Queued` for hours when in fact the evaluation is fully decomposed and waiting on `aarch64-linux` workers that don't exist.

**Fix:** the cleanest version is to introduce the right state diagram up front:

```text
Queued (eval not yet started)
  └── Fetching → EvaluatingFlake → EvaluatingDerivation
        └── Waiting (eval done; no compatible worker for at least one build)
              └── Building (worker claimed at least one)
                    └── Completed | Failed | Aborted
```

…and have `reconcile_waiting_state` *also* re-evaluate `Queued` rows that have ≥1 build attached (i.e. eval finished but somehow status didn't advance). Mechanically: extend the `is_in([Building, Waiting])` filter to include `Queued` rows where `EXISTS(SELECT 1 FROM build WHERE build.evaluation = e.id)`. The transition-decision logic (`any_buildable`) is already correct.

### 24.6 [HIGH] API returns no reason for `Waiting` state — confirmed missing
**Files:** `backend/web/src/endpoints/evals/query.rs:24-…` (`get_evaluation`), `backend/web/src/endpoints/evals/types.rs::EvaluationResponse`, `backend/scheduler/src/build.rs:282-302`.

`get_evaluation` returns `EvaluationResponse { status: EvaluationStatus, … }` — a bare enum. There is **no `status_reason: Option<String>`** field, no `waiting_for: Vec<{architecture, system_features}>` structure. The reconciler at `build.rs:288-302` does compute the right info:

```rust
let target = if checker.any_buildable(&pending_builds, worker_caps) {
    EvaluationStatus::Building
} else {
    EvaluationStatus::Waiting
};
if eval.status != target {
    info!(
        evaluation_id = %eval.id,
        from = ?eval.status,
        to = ?target,
        pending = pending_builds.len(),
        workers = worker_caps.len(),
        "reconciling evaluation waiting state"
    );
    update_evaluation_status(...).await;
}
```

…but **only as a tracing log line**. Nothing reaches the DB row, the `evaluation_message` table, or the API response. A user looking at the UI sees "Waiting" with no explanation, while a sysadmin watching logs sees the architecture mismatch — exactly the wrong split of information.

**Fix:** two complementary changes:

1. **Persist a structured reason on transition.** Add columns or use the existing `evaluation_message` table — log a `MessageLevel::Notice` row with `source = "scheduler:waiting"` and a typed payload like `{"reason": "no_compatible_worker", "needed_systems": ["aarch64-linux"], "needed_features": []}`. Each reconcile that *changes* status writes one. The API renders the latest such message.
2. **Surface in `EvaluationResponse`:**

```rust
pub struct WaitingReason {
    pub kind: String,                 // "no_workers" | "incompatible_arch" | "missing_features"
    pub needed_systems: Vec<String>,
    pub needed_features: Vec<String>,
    pub workers_seen: Vec<WorkerSummary>, // optional, for the UI tooltip
}

pub struct EvaluationResponse {
    ...,
    pub waiting_reason: Option<WaitingReason>,
}
```

The reason is non-trivial to compute exactly because `BuildabilityChecker::any_buildable` returns `bool`. Extend it to return `Result<(), BuildabilityFailure>` where the error variant carries the missing capability set. Same data, richer signal. Tied to §22.11 — operator-facing telemetry per concern.

### 24.7 [HIGH] Workers exit on reconnect failure — confirmed (contradicts intent)
**File:** `backend/worker/src/main.rs` (around line 155, in the `Run → reconnect` loop).

Current code:

```rust
loop {
    let (disconnected, outcome) = worker.run().await;

    match outcome {
        Ok(RunOutcome::Drained) => {
            info!("server requested drain; shutting down");
            break;
        }
        Ok(RunOutcome::CleanDisconnect) => {
            warn!(delay_secs = backoff.as_secs(), "connection closed; reconnecting");
        }
        Err(e) => {
            error!(error = %e, delay_secs = backoff.as_secs(), "dispatch loop error; reconnecting");
        }
    }

    tokio::time::sleep(backoff).await;

    match disconnected.reconnect().await {
        Ok(w) => { worker = w; backoff = INITIAL_BACKOFF; info!("reconnected successfully"); }
        Err(e) => {
            error!(error = %e, "reconnect failed; will retry");
            // Loop will break because `worker` has been moved — exit gracefully.
            break;
        }
    }
}
```

The comment claims "will retry" but the code says `break` — the worker process exits. The discrepancy means: any transient `reconnect` failure (TLS handshake hiccup, DNS blip, server reboot, network partition) **terminates the worker permanently**. systemd will restart it (if the operator wired `Restart=on-failure`); otherwise the build fleet just shrinks one worker per blip until it's gone.

This contradicts the user's stated intent — workers should keep trying forever.

**Fix:** keep the disconnected `Worker<Disconnected>` value around and retry with capped exponential backoff, never breaking the outer loop:

```rust
let mut disconnected = ...;
loop {
    tokio::time::sleep(backoff).await;
    match disconnected.reconnect().await {
        Ok(w) => { worker = w; backoff = INITIAL_BACKOFF; break; }
        Err(e) => {
            error!(error = %e, delay_secs = backoff.as_secs(), "reconnect failed; retrying");
            backoff = (backoff * 2).min(MAX_BACKOFF);
            // disconnected is reusable — retry
        }
    }
}
```

…or restructure as a state machine where `Disconnected → reconnect()` returns `Result<Connected, Disconnected>` with the value preserved on Err. Add jitter (`+/- 20%`) to the backoff, and emit a `warn!` at every Nth retry so operators can correlate with their network metrics. The current `worker_id` should be loaded at startup and reused, so reconnects are idempotent (already true per `do_reconnect_handshake` in `worker/mod.rs:142`).

Bonus: distinguish "server returned protocol-level rejection" (→ stop, don't retry) from "transport failure" (→ retry forever). Today both end up as `Err(anyhow)`; the failure-mode taxonomy is missing.

### 24.8 [MEDIUM] CI reporter is not injectable in tests — confirmed
**Files:** `backend/scheduler/src/ci.rs:9, 63, 182` (calls `resolve_outbound_reporter_for_project`), `backend/core/src/ci/integration_lookup.rs:79-178` (`resolve_outbound_reporter_for_project` reads from DB + `state.cli`), `backend/scheduler/src/handler_tests.rs:751-…` (cascade tests).

`resolve_outbound_reporter_for_project` constructs a `Arc<dyn CiReporter>` from:

- `EProjectIntegration` row look-up.
- `EIntegration::find_by_id` look-up (with FK).
- `state.cli.crypt_secret_file` read for token decryption.
- `state.cli.github_app_config()` look-up.
- `fs::read_to_string(github_app.private_key_file)`.

Every call is a hard dependency on real DB rows, real config, and a real PEM on disk. In `MockDb`-backed tests, all of these short-circuit to `Arc::new(NoopCiReporter)` — so tests can never assert "did we call `report(CiStatus::Success)` when the eval went green?".

This blocks the **L2/L3/Q1/Q2** test labels mentioned by the user (likely terminal-state CI reporting matrix). The current cascade tests in `handler_tests.rs:751-775` inspect the final `Failed` evaluation status but cannot prove the CI step *also* fired.

**Fix:** introduce a factory trait and inject it through `Scheduler` / `ServerState`:

```rust
#[async_trait]
pub trait CiReporterFactory: Send + Sync + 'static {
    async fn for_project(&self, state: &ServerState, project_id: Uuid) -> Arc<dyn CiReporter>;
}

pub struct DbBackedCiReporterFactory;       // production: today's resolve_outbound_…
pub struct StaticCiReporterFactory(pub Arc<dyn CiReporter>);  // tests: returns whatever you set
```

…and store `Arc<dyn CiReporterFactory>` on `ServerState` (or on `Scheduler` if the scope is narrower). Production builds wire `DbBackedCiReporterFactory`; test scaffolding (`test-support`) hands tests a `RecordingCiReporter` that pushes every `report(...)` call onto a `Vec<CiCall>` for assertion.

This is a **pure** quality-of-tests change — no runtime behavioural shift in production. Pair with the `Provider<T>` DI pattern in §19.24 and the `BackgroundJobs` registry (§21.3): three injection points, the same shape.

### 24.9 [MEDIUM] Transitive dependency-failed cascade is hard to test against `MockDb` — confirmed
**Files:** `backend/scheduler/src/build.rs:129-176` (`cascade_dependency_failed`), `backend/scheduler/src/handler_tests.rs:751-775`.

`cascade_dependency_failed` does an iterative graph walk:

```rust
async fn cascade_dependency_failed(&self, eval_id: Uuid, drv_id: Uuid) -> Result<()> {
    // Step 1: find direct dependents (builds whose derivation depends on `drv_id`).
    // Step 2: mark them DependencyFailed.
    // Step 3: recurse: for each newly failed build, find its dependents, mark, recurse.
    ...
}
```

Each level is a fresh `EBuild::find()` / `EDerivationDependency::find()` call. `MockDb` requires the test to pre-stage every query result in order, with `append_query_results([...])` lines per query. A cascade across 3 transitive levels needs ~6 staged queries in exactly the right order; small refactors of `cascade_dependency_failed` reorder the queries and silently invalidate the mocks.

**Two-pronged fix:**

1. **Refactor for testability**: extract a `DependencyGraph` struct that takes `&[MBuild]` + `&[MDerivationDependency]` *up front* (one query each) and computes the transitive closure in pure code. The scheduler does one snapshot read, then calls a pure function. Tests build the graph in-memory; no `MockDb` choreography.
2. **Test against an ephemeral PG via testcontainers** for the integration suite: real cascades, real FK constraints, real concurrent updates. `MockDb` is fine for unit-shape tests but breaks down on graph algorithms that read a lot of rows.

The pure-function refactor is also faster in production (one query for the graph + one batch UPDATE for the cascade vs N queries today). Performance + testability — same change.

---

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

