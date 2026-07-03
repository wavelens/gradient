# AUDIT-WEB.md - Web / Server Layer & Cross-Cutting Concerns

Scope: `backend/gradient-web` (~29k LOC of production code across 96 files), `backend/src` (binary entrypoint), and cross-cutting patterns across all crates. Counts below are from grep runs against the tree at the time of audit. Produced by a multi-agent code audit.

Headline: contrary to a "messy giant files everywhere" premise, the web layer is mostly well-engineered. The concentrated messiness is one file (`trigger.rs`), one inconsistent handler, a dead module, and minor boilerplate. This file records both the strengths (as patterns to emulate elsewhere) and the real defects.

---

## Web layer - architecture overview

The two headline files are large for different reasons: `access.rs` is 63% tests, while `trigger.rs` is 85% production code and is the genuine outlier.

Module layout (`backend/gradient-web/src/`):
- `lib.rs` (875) - router assembly (`create_router`) + server lifecycle (`serve_web`), signal handling, background-loop startup.
- `error.rs` (474) - the single typed HTTP error (`WebError`) + stable `ErrorCode` slugs.
- `access.rs` (1492, ~554 prod / ~938 test) - unified resource-load + authorization layer.
- `authorization/` - `middleware.rs` (session/optional auth), `api_key.rs`, `jwt.rs`, `oidc.rs` (649), `scim.rs`.
- `endpoints/` - 73 files grouped by resource (`orgs/`, `projects/`, `caches/`, `builds/`, `evals/`, `board*`, `forge_hooks/`, `build_requests/`, `admin/`, `scim/`).
- Shared helpers: `helpers.rs` (`ok_json`, `OptionExt::or_not_found`), `permissions.rs`, `audit.rs`, `client_ip.rs`, `ip_allowlist.rs`, `metrics_scope.rs`.

Request path (outer to inner middleware; `lib.rs:760-770`):

```
TCP (into_make_service_with_connect_info::<SocketAddr>)
  +- SetRequestIdLayer (x-request-id = UUID v7, lib.rs:82-93)
     +- TraceLayer (info_span http_request {method, route=MatchedPath, request_id}, lib.rs:156-204)
        +- PropagateRequestIdLayer
           +- CORS (allowlist: serve_url + debug_url)
              +- DefaultBodyLimit + GovernorLayer (per-IP token bucket, tiered)
                 +- track_http_metrics (lib.rs:656)
                    +- route-tier auth middleware:
                       - authorize          inserts MUser, MaybeApiKey, ClientIp  (auth_api tier)
                       - authorize_optional inserts MaybeUser, MaybeApiKey, ClientIp (optional_api tier)
                       - authorize_scim     bearer token (scim/v2)
                       - (none)             HMAC-verified webhooks / fully public
                       +- HANDLER
                          +- access::load_{org,project,cache}(state, Caller, api_key, name, Access)
                               returns fully-validated MOrganization/MProject/MCache
                          +- Ok(ok_json(payload))  |  Err(WebError) -> {error, code, message}
```

Route tiers are declared as separate `Router`s and merged (`lib.rs:207-642`): `auth_api` (mandatory session), `optional_api` (public-browsable), `auth_sensitive` (login/oauth, tight 6 r/s governor), `webhook_routes` (unauthenticated, HMAC), plus fully-public `/health`, `/config`, `/orgs/public`. NAR cache surface, SCIM, and the Prometheus `/metrics` route are conditionally mounted based on `state.config.*.is_some()`.

State is a single `Arc<ServerState>` (defined in `gradient-core/src/lib.rs`) threaded via axum `State`, plus two `Extension`s (`Arc<Scheduler>`, `Arc<ProtoLimiter>`). Config is resolved once at boot from the clap `Cli` DTO into a typed `RuntimeConfig` (`gradient-types/src/config.rs:217-263`) where optional features (`oidc`, `email`, `s3`, `github_app`, `metrics`, `scim`) collapse to `None` unless fully configured. This is a clean pattern.

Access control is the strongest part of the codebase. `access.rs` exposes `load_org`/`load_project`/`load_cache` taking a `Caller` (`Anon` | `User`), an optional `ApiKeyContext`, and an `*Access` policy enum (`Readable`/`Member`/`Require{permission, reject_managed}`). Authorization is expressed as capability masks (`mask_grants`), API-key masks are intersected with role masks, org/cache pinning short-circuits, and state-managed rows are rejected for mutations. It is adopted at 106 call sites (`load_org` 38, `load_project` 32, `load_cache` 36) and backed by ~938 lines of mock-DB unit tests.

---

## Cross-cutting concerns

**Error-handling strategy - consistent and typed at the boundary (a model to emulate).**
- HTTP boundary uses one typed enum, `WebError` (`error.rs:108-137`), with a `WebResult<T>` alias, a stable machine-readable `ErrorCode` newtype (~45 slugs, `error.rs:36-99`), `From<DbErr>`/`From<JsonRejection>`/`From<InputError>` conversions, ~30 domain constructors, and a single `IntoResponse` that emits `{error, code, message}` JSON. Internal 500s log once at `error!`; the `DataInconsistency` variant is a deliberate warn-level 500 for referential races.
- 3 HTTP error types total, all justified: `WebError` (general), `ScimError` (`scim/error.rs:15`, RFC 7644 mandates a different `scimType` envelope), and `IpAllowlistError` (`ip_allowlist.rs:25`, internal). No error-type sprawl in the web layer.
- Workspace-wide there are 36 distinct `*Error` enums/structs (non-test), one typed error per crate boundary (`InitError`, `ConfigError`, `SourceError`, `ApplyError`, `TriggerError`, `BuildError`, `StorePathError`, `NarInfoParseError`) with `thiserror`, and anyhow used internally (105 files import it, 102 `anyhow::Result` signatures). This anyhow-internal / typed-at-boundary split is the correct idiom, not a smell.

**unwrap / expect / panic - not a real risk in the web layer.**
- `.unwrap()`: 1132 backend-wide (non-tests), but hotspots are `gradient-worker` (276), `gradient-proto` (128), `gradient-forge` (83), all outside this file's scope, and 476 more live under `tests/`. `gradient-web` shows only 39, and spot-checks of the worst files prove they are all in inline `#[cfg(test)]` modules (`board.rs:1010-1015`, `client_ip.rs:103/107/112`, `builds/log_chunks.rs:278-297`).
- `panic!`/`unreachable!`: 64 in source (non-target), every one inside a `#[cfg(test)]` module (verified: `access.rs:1004`, `trigger.rs:2063/2072/2081`, `helpers.rs:58`). Zero production panics.
- `.expect()`: 277 backend-wide. The web layer's worst file is `endpoints/metrics.rs` (30), all idiomatic Prometheus `Gauge::new(...).expect("metric")` registrations that run once with static names. The only production `.expect()` that panic at runtime are 4 boot-time invariants in `lib.rs:108/123/138/141` (governor config, `serve_url`, `debug_url`); they run after `init_state` so config is already validated, low risk, but should surface as `InitError`/`io::Error` from `serve_web` rather than panicking the router builder.
- `unimplemented!`/`todo!`/`dbg!`: 0 (the workspace lints `todo`/`dbg_macro` as warnings).

**TODO/FIXME/HACK inventory - trivially small.** Only 4 TODO in the entire source tree (all FIXME hits are in `target/` artifacts, HACK/XXX = 0):
- `gradient-web/src/endpoints/caches/management.rs:104` - `// TODO: Implement pagination` (the `GET /caches` list is unbounded).
- `gradient-web/tests/cache_nar_delete.rs:39` - `TODO(#260)` needs real-DB harness.
- `gradient-worker/src/executor/eval.rs:231` - `TODO(#386)` cache_status reporting.
- `gradient-state/src/provisioning/entities/projects.rs:844` - missing integration test.

**Config / env-var handling - clean.** Single source of truth: clap-derived `Cli` with 108 `env =` attributes across `gradient-types/src/cli/*.rs`, resolved once into `RuntimeConfig`. Secrets are loaded from `*_file` paths (`load_secret`), never raw env. `ConfigError` names the offending env var in its `Display` (`config.rs:200-205`). `RUST_LOG` overrides a synthesized per-crate filter directive with dependency-noise suppression (`src/main.rs:23-59`).

**Logging / tracing - consistent.** `tracing` is used uniformly; zero stray `println!`/`eprintln!` in library code (the 5 `eprintln!` are all in `src/main.rs` bootstrap/`--validate-state`, correct pre-subscriber behavior). Every request gets a span with `request_id`, and there is a dedicated `target: "audit"` structured event stream (`audit.rs:114-145`) alongside DB-persisted audit rows for 30+ security events.

**Code duplication - mostly well-factored, a few gaps.** `ok_json()` is used 183x vs only 29 raw `Json(BaseResponse{..})` literals; the `Caller` prologue appears 106x. Remaining duplication: batch role-id-to-name maps, hand-rolled pagination, and one endpoint that bypasses the shared access loader.

---

## Messiness & code smells

Ranked by impact.

**1. `endpoints/forge_hooks/trigger.rs` (2173 lines) - the one genuinely oversized module.** 85% production code (tests only start at `trigger.rs:1838`). It mixes webhook-payload DTO parsing, DB access, business logic, forge-API side effects, and response building in one file. Refactor targets:
- `handle_issue_comment` (`trigger.rs:1317-1614`, ~298 lines) - the single worst function. In one body it deserializes 3 different forge comment payloads (GitHub/Gitea/GitLab, `~1333-1364`), parses `/gradient run|approve`, resolves the integration with IP-allowlist checks, runs `sender_is_trusted`, fires an emoji reaction, then either unparks a PR-approval eval or creates a fresh evaluation, and posts error comments.
- `fan_out_triggers` (`trigger.rs:533-703`, ~171 lines) - nested for trigger, parse config, DB lookup, FilterResult match (`:520-530`), gate decision, apply, touch_last_fired, accumulate response. Five concerns in one loop; `org_name_for` is re-queried inside the loop (`:618-620, 671, 679, 687, 696`).
- `handle_pull_request_review` (`:1143-1232`, ~90 lines), `handle_github_check_run` (`:908-986`, ~79 lines), `resolve_github_app_targets` (`:268-345`, ~78 lines) - same parse+resolve+trust+unpark shape.
- Four near-duplicate dispatchers: `trigger_push_for_integration` (`:358`), `trigger_pr_for_integration` (`:409`), `trigger_release_for_integration` (`:469`), all funnel into `fan_out_triggers`.
- ~11 inline webhook DTO structs scattered through the file (`GithubCheckRunRef`/`GithubSender` `:872-905`, `CommentPayload`/`CommentBody`/`CommentIssue`/`CommentSender`/`CommentRepo` `:1237-1288`, `GitlabNoteAttrs`/`GitlabNoteProject`/`GitlabNoteMr` `:1288-1310`).

**2. `caches/management.rs::get` (`:99-137`) bypasses the shared access layer.** It hand-rolls org-membership to cache-visibility resolution (`EOrganizationUser::find` + a `Condition::any()`) instead of going through `load_cache`/`effective_cache_mask`, unlike every sibling in `caches/roles.rs`/`caches/members.rs`. This is an authorization-consistency risk: visibility logic now lives in two places and can drift. Same file carries the pagination TODO at `:104`.

**3. Dead module: `gradient-web/src/requests.rs` (71 lines).** Not declared with `mod requests` in `lib.rs` (the module list at `lib.rs:7-21` omits it), so it does not compile into the crate. It contains stale request DTOs (`MakeOrganizationRequest`, `MakeServerRequest`) superseded by per-endpoint types. Delete it.

**4. Repeated boilerplate (small but pervasive).**
- Batch role-id-to-name map duplicated at `orgs/members.rs:96-102` and `orgs/management.rs:172-178` (identical `ERole::find().filter(Id.is_in(role_ids))...collect::<HashMap>`).
- Hand-rolled pagination (`params.page()/per_page()` to `.paginate()` to `num_items()` + `fetch_page()`) repeated in `orgs/management.rs:143-160` and `projects/management.rs:105-135`; 22 separate `*Query`/`*Params` structs across endpoints with no shared paginator.
- 29 raw `BaseResponse{..}` literals remain where `ok_json` would do.

**5. Module-organization / test-coverage observations.**
- `access.rs` (1492) is not a smell: it is 63% unit tests (`:555-1492`) with excellent per-policy coverage; production code is only ~554 lines. Test fixtures could move to a `tests/` submodule to shrink the file, but the code itself is exemplary.
- Integration coverage is strong: 47 test files in `gradient-web/tests/` (auth hardening, rate limiting, IP allowlist, SCIM, cache pinning, forge hooks, triggers). See AUDIT-TEST.md for the compaction opportunities in that suite.
- 51 of 73 endpoint files have no inline `#[cfg(test)]` module; combined with the integration suite this is acceptable, but `trigger.rs`'s payload parsers and `caches/management.rs`'s hand-rolled visibility query deserve direct unit tests.

---

## Refactoring recommendations

Ordered by impact-to-effort.

**1. Decompose `trigger.rs` into a `forge_hooks/` submodule tree (highest impact).**
- Extract all webhook payload DTOs into `forge_hooks/payloads.rs` (or push them into `gradient-forge`, which already owns `ParsedPushEvent`/`ParsedPullRequestEvent`), so `trigger.rs` stops interleaving serde structs with logic.
- Split by concern: `installation.rs` (`handle_github_installation`, `store/clear_installation_id`, `resolve_github_app_targets`), `fanout.rs` (`fan_out_triggers` + `FilterResult` + the three `trigger_*_for_integration`), `approval.rs` (check-run/PR-review/`sender_is_trusted`/`unpark_pr_approval_eval`), `commands.rs` (`parse_gradient_command`, `handle_issue_comment`).
- Inside `handle_issue_comment`, extract a `parse_comment_event(forge, body) -> CommentEvent` normalizer so the 3 forge branches collapse to one, then split the "unpark vs fresh-eval" tail into two named functions. Target: no function over ~80 lines.

**2. Unify HTTP error surfaces minimally.** `WebError` is already excellent, leave it. Two touch-ups: (a) give `ScimError` a `From<WebError>` (or a shared `HttpStatusError` trait) so SCIM handlers can reuse `access.rs` loaders without manual re-mapping; (b) replace the 4 boot-time `.expect()` panics in `lib.rs:108/123/138/141` with `Result` propagation out of `create_router`/`serve_web`.

**3. Add shared endpoint helpers for the two remaining duplication patterns.** A `helpers::paginate(query, finder, db) -> (items, total)` wrapper (folding the 22 ad-hoc `*Query` structs onto one `Pagination` extractor), and a `helpers::role_names(db, role_ids) -> HashMap<RoleId, String>` for the batch lookup at `orgs/members.rs:96` and `orgs/management.rs:172`.

**4. Access-control cleanup.** Route `caches/management.rs::get` (`:99-137`) through `load_cache`/`effective_cache_mask` so cache visibility has a single implementation; then implement the `:104` pagination TODO via the new helper.

**5. Delete dead code.** Remove `gradient-web/src/requests.rs` (unreferenced).

Bottom line: the web layer is in good shape - typed errors, disciplined unwrap/panic hygiene (zero production panics), a clean once-resolved config, consistent tracing, and a well-designed, well-tested access-control layer adopted at 106 sites. The concentrated messiness is one file (`trigger.rs`), one inconsistent handler (`caches/management.rs::get`), a dead module (`requests.rs`), and minor boilerplate. The alarming workspace-wide unwrap/panic density (1132/64) lives almost entirely in tests and in the worker/proto/forge crates, not here.
