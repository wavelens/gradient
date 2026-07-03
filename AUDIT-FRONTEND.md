# AUDIT-FRONTEND.md - Frontend (Angular)

## Scope

The web frontend: `frontend/` - Angular 21 (`@angular/core ^21.2.16`, TypeScript ~5.9), ~38k LOC across ~313 `.ts`/`.html`/`.scss` files, built with pnpm. App code lives under `frontend/src`; `frontend/dashboard/` holds two unrelated Django/Python helper files (`api.py`, `auth.py`), not part of the Angular app.

Files/areas in scope:
- `frontend/src/` app: routing, components (73), services (21), the API client, auth/session handling, state management.
- `frontend/proxy.conf.json` (dev proxy `/api` -> `localhost:3000`, `ws: true`), `angular.json`, `tsconfig*`.
- The API contract surface against `docs/gradient-api.yaml` and the backend endpoints (AUDIT-WEB.md).

Counts below are from grep/wc runs against the tree at audit time. Source LOC (`src/**/*.ts` excluding specs) is ~16.7k TS + ~7.7k HTML + ~8.7k SCSS. Headline: the frontend is in decent shape - fully standalone components, lazy routes everywhere, strict TypeScript, near-zero `any`, signals for local state. The real defects are one god-component, two parallel WebSocket implementations, an API layer that silently discards the backend's stable error `code`, DTO types with no single home, and the absence of a shared async-fetch abstraction (315 hand-rolled subscribe blocks).

---

## Frontend - architecture overview

**Bootstrap and DI.** Standalone bootstrap (`src/main.ts` -> `app/app.ts` root `App` component). No NgModules anywhere. `app/app.config.ts` wires `provideRouter` (with `paramsInheritanceStrategy: 'always'`), `provideHttpClient(withInterceptors([authInterceptor, errorInterceptor]))`, `provideAnimations`, PrimeNG (`providePrimeNG` + Aura preset, `.app-dark` selector), a `TitleStrategy`, and an `APP_INITIALIZER` that runs `ConfigService.load()` before the app starts. DI is uniformly the `inject()` function form; all 21 services are `@Injectable({ providedIn: 'root' })` singletons.

**Component model.** 73 standalone components, feature-foldered under `src/app/features/{auth,board,caches,organizations,projects,evaluations,settings,admin,dashboard,errors,styleguide}`, with cross-cutting pieces under `src/app/shared/` (form kit, layout, badges, access directives) and `src/app/core/` (services, models, guards, resolvers, interceptors, title). 53 components use `templateUrl`; 19 use inline templates (the board sub-views). Local UI state is signals (58 files use `signal`/`computed`/`effect`/`input`/`output`).

**Routing.** `app/app.routes.ts` (509 lines) is one flat `Routes` array, entirely lazy (`loadComponent: () => import(...)`). Tiers: public (`account/*`, `organization/:org`, `caches/:cache`, list pages), and an authenticated block guarded by `authGuard` (dashboard, `board/*`, org settings/members/workers/integrations, user settings). `admin/github-app` adds `adminGuard` + `unsavedChangesGuard`. Parent layout routes (`project-layout`, `cache-layout`) attach access `resolve`rs with `runGuardsAndResolvers: 'paramsChange'`; `error/{500,502,503,504}` and `**` render one shared `ErrorPageComponent` keyed by `data.code`.

**Access / authorization on the client.** Route resolvers (`core/resolvers/{project,cache,organization}-access.resolver.ts`) fetch the entity and derive an `AccessState` (`accessFromEntity`); `inject-access.ts` exposes it to components as a signal; `shared/access/` provides `AccessService` (policy helpers), and `*appWritable` / `[appManagedDisable]` structural/attribute directives that gate write UI. This mirrors the server's capability model (AUDIT-WEB.md access layer) but is presentation-only - the backend remains the enforcement point.

**Server state: no store, per-component fetch.** There is no NgRx / signal-store / query-cache. Each component injects the relevant `*Service`, calls it in `ngOnInit`, and drops the result into local `signal`s. The only server-state caching is one `shareReplay` on `BoardService.getScoringRules()` (`board.service.ts:377-383`) and the `APP_INITIALIZER`-loaded `ConfigService`. Re-navigating a route re-fetches.

**The API client (hand-written, not generated).** One generic `ApiService` (`core/services/api.service.ts`, 87 lines) wraps `HttpClient`: `request<T>(method, endpoint, body)` builds `${environment.apiUrl}/${endpoint}`, unwraps the `{error, message}` envelope (`map` returns `response.message as T`, throws on `response.error`), and `catchError`s into a plain `Error(message)`. `get/post/put/patch/delete` are thin shims. Every feature service (`projects`, `organizations`, `caches`, `evaluations`, `board`, `workers`, `triggers`, `integrations`, `actions`, `admin`, `user`, `flake-input-overrides`) is a hand-written set of one-line methods that string-interpolate the endpoint and pick a return type. DTOs are TypeScript `interface`s written by hand - there is no OpenAPI codegen despite `docs/gradient-api.yaml` (10.4k lines) existing as a contract.

**Auth / session.** JWT lives in an httpOnly cookie; JS never reads it. `authInterceptor` adds `withCredentials: true` to `/api/v1` requests. `AuthService` holds `user`/`token`/`loading` signals plus an `initialized$` gate, probes `GET /user` on construction, and exposes `login`/`register`/`logout`/`loginWithCookie`. `errorInterceptor` centralizes 401 (clear storage, redirect to `/account/login?next=`), 0/502/503/504 (render error page via `skipLocationChange` so F5 retries the original route), and logs 403/5xx.

**Real-time updates.** Two separate WebSocket services: `LiveService.connect(path)` (per-resource channels like `/evals/{id}/live`, with capped exponential-backoff reconnect) and `BoardLiveService.connect()` (the `/board/live` fleet feed, no reconnect, completes on close). Neither uses SSE. Several components additionally poll with `setInterval`/rxjs `interval` (project-detail 1s tick, evaluation-log duration/reveal/drain timers, dependency-graph). `EvaluationLogComponent` streams build logs with raw `fetch()` + `ReadableStream` (5 call sites), bypassing `ApiService` and the interceptors.

Data flow (route -> component -> service -> API -> render):

```
  URL change
     |
     v
  Router (app.routes.ts, lazy loadComponent)
     |  runs canActivate guards (authGuard/adminGuard)
     |  runs resolve (projectAccessResolver -> ProjectsService.getProject)
     v
  Component (ngOnInit)
     |  this.svc.getX().subscribe({ next -> signal.set(data),
     |                              error -> errorMessage.set(msg) })
     v
  Feature service (ProjectsService / BoardService / ...)
     |  api.get<T>('projects/{org}/{proj}/details')
     v
  ApiService.request<T>()
     |  HttpClient.request<ApiResponse<T>>()  + authInterceptor (withCredentials)
     |  unwrap {error,message} -> T   (drops `code` on error)
     v
  Backend  (BaseResponse<T> = {error,message};  errors = {error,code,message})
     |
     v
  signal set -> template (@if/@for, async of computed signals) -> DOM

  Live overlay (parallel):
  Component --> LiveService.connect('/evals/{id}/live')  (WS + backoff)
            --> BoardLiveService.connect()               (WS, no backoff)
            --> setInterval / interval(...)              (polling)
            --> fetch(.../log) + ReadableStream          (log streaming, no interceptors)
```

---

## Messiness & code smells

Ranked by impact. File:line against `main` at audit time.

**1. `evaluations/evaluation-log/evaluation-log.component.ts` (1349 lines) - the one true god-component.** It is 2.7x the next-largest file and fuses at least seven concerns in one class: a virtualized log window (`windowLines`/`topSpacerPx`/`bottomSpacerPx` signals + `log-window.ts` helpers), in-memory streaming vs completed-build chunk fetching (`chunkedMode`, `PAGE_SIZE`), five `setInterval` timers (`durationInterval:113`, `buildRevealTimer:121/921`, `logDrainTimer:123/948`, plus a duration tick `:1181`), a raw `fetch()` log streamer at five sites (`:558,:596,:659,:697,:866`) with a manual `ReadableStreamDefaultReader`, WS live wiring via `LiveService`, `DomSanitizer` HTML rendering, keyboard/search handling (`keyboard.ts`, `build-search.ts`), and manual `ChangeDetectorRef` + `Subscription` bookkeeping. It mixes a raw-`fetch` transport (bypassing `authInterceptor`/`errorInterceptor`) with the `ApiService` path in the same file. This is the frontend analogue of `trigger.rs` (AUDIT-WEB.md smell 1).

**2. Two parallel WebSocket implementations with divergent reliability.** `core/services/live.service.ts` (69 lines) reconnects with capped exponential backoff; `core/services/board-live.service.ts` (49 lines) does not reconnect at all (`onclose -> subscriber.complete()`, `board-live.service.ts:41`), so the live job board silently goes stale after any transient socket drop. The two share ~30 lines of near-identical `new WebSocket(proto + host + apiUrl + path)` + JSON-parse + teardown logic. On top of that, every consumer re-implements its own subscription/reconnect/refresh bookkeeping: `liveSub?: Subscription` fields with manual `ngOnDestroy` teardown appear in `evaluation-log`, `dependency-graph`, `project-detail`, `board/cache`, `board/live-jobs`, `board/overview`.

**3. The API layer discards the backend's stable error `code` (contract drift).** The backend returns errors as `{error: true, code, message}` where `code` is a stable machine-readable `ErrorCode` slug (`backend/gradient-web/src/error.rs:217-248`, ~45 slugs; AUDIT-WEB.md cross-cutting). The frontend models the envelope as only `{error, message}` (`core/models/api-response.model.ts:7-10`), `ApiService.request` throws `new Error(response.message as string)` and `catchError`s to `error.error?.message || error.message` (`api.service.ts:40-49`), and `errorInterceptor` branches solely on HTTP status (`error.interceptor.ts:34-64`). The `code` field is never read anywhere in `src/`. Result: all client error handling is fragile English-string matching, and the deliberate machine-readable contract is thrown away.

**4. DTO types have no single home - split ~50/50 between `models/` and inline in services.** `core/models/*.model.ts` defines 79 exported types; the service files define ~75 more inline - `board.service.ts` alone declares 37 interfaces (`board.service.ts:11-336`) before the class, `evaluations.service.ts` 10, `caches.service.ts` 10, `projects.service.ts` 4 (metric types tacked on *after* the class, `:104-136`). A reader cannot predict whether a response type lives in `models/` or beside the service.

**5. Board DTOs carry raw-int enums, propagating the backend's bare-int smell into the templates.** `DispatchedJobSummary.kind: number`, `PendingJobSummary.kind: number`, `LiveEvent.status: number`, `BoardLiveEvent.kind: number` are untyped ints (`board.service.ts:12`, `:29`; `live.service.ts:22`; `board-live.service.ts:16`). Components then hard-code the mapping: `kind === 1 ? 'build' : 'eval'` appears 15 times across `live-jobs.component.ts` and `job-detail.component.ts` (e.g. `job-detail.component.ts:43-44`, `live-jobs.component.ts:84,105,130,216-217`). Contrast `build.model.ts:20-30`, where `BuildStatus` is a proper string-union - the good pattern that the board DTOs ignore.

**6. No shared async-fetch abstraction - 315 hand-rolled subscribe blocks.** There are 315 `next:`/`error:` handler blocks and 207 `.subscribe(` calls; 32 components declare a `loading = signal(...)` and 16 an `error*/errorMessage = signal(...)`, each re-implementing the same load/loading/error triple by hand. Angular 21's `httpResource()` / `resource()` / `rxResource()` are used zero times. This is the single largest source of repetitive boilerplate.

**7. Subscription-teardown hygiene is inconsistent (leak risk).** Only 2 files use `takeUntilDestroyed`/`DestroyRef` (`evaluation-log`, `job-detail`); the other long-lived subscriptions rely on manual `liveSub?.unsubscribe()` in one of just 10 `ngOnDestroy` methods. Any component that forgets (or that adds a subscription without a matching teardown) leaks a WebSocket or interval. The `interval(1000)` ticker in `project-detail.component.ts:112` is manually torn down; the pattern is easy to get wrong at scale.

**8. `config.service.ts` breaks two conventions at once.** It bypasses `ApiService` and calls `HttpClient` directly, re-declaring the `{error, message}` envelope inline (`config.service.ts:52-56`), and it stores server config as plain mutable fields (`oidcEnabled`, `createOrg`, ...) instead of signals (`:33-40`), unlike the signal-based `AuthService`. Components that read these fields do not react if the values ever change.

**9. Raw `fetch()` outside the interceptor chain (6 sites).** Beyond the five in `evaluation-log`, `cache-upstreams.component.ts:242` probes an upstream `gradient-cache-info` endpoint with bare `fetch`. These calls skip `authInterceptor` (must re-add credentials by hand) and `errorInterceptor` (no 401 redirect / error-page handling), so error behavior diverges from the rest of the app.

**10. `any` casts concentrated in un-typed forge config (9 sites).** All 9 non-spec `any` uses are in trigger/integration/worker config building: `project-triggers.component.ts:180,217,226,243,265,319`, `integrations.component.ts:243,293`, `workers.component.ts:251`. The trigger/integration `config` is a discriminated union on the backend (`reporter_push` / `reporter_pull_request` / ...) but is modeled as an opaque blob on the client, forcing `as any` to build and read it. Otherwise typing discipline is excellent (strict mode on, `noPropertyAccessFromIndexSignature`, `strictTemplates`).

**11. Minor drift / staleness.**
- `APP_INITIALIZER` is deprecated in Angular 21 in favor of `provideAppInitializer` (`app.config.ts:7,39`).
- `environments/environment.ts` (prod) and `environment.development.ts` disagree: the dev file adds `emailVerificationEnabled` that prod omits, and both hard-code `oidcEnabled/registrationDisabled` that are actually resolved at runtime by `ConfigService` - the env flags are dead.
- Duplicate pagination shapes: `Paginated<T>` (`api-response.model.ts:12`) vs bespoke `{total, ...}` inline responses (`PaginatedBuilds` in `evaluations.service.ts:75`, `NarListResponse` in `caches.service.ts`).
- The `styleguide` route (289-line TS + 662-line HTML) is intentionally unlinked but still compiled into a lazy chunk shipped to prod; consider excluding it from prod builds. (`dist/` is correctly gitignored - not tracked.)

**12. Test coverage is thin on the API surface.** 53 spec files vs 139 non-spec `.ts`. Core services with specs: `board`, `admin`, `org-access`, `live`. Untested: `api.service` (the generic wrapper every call flows through), `auth.service`, `projects`, `evaluations`, `caches`, `organizations`, `workers`, `config`. The most load-bearing and most-reused code (the envelope unwrap, auth session) has the least direct coverage.

---

## Refactoring recommendations

Ordered by impact-to-effort; aligned with the "one legible flow" north star (AUDIT.md) - a reader should open one place and see how data gets from route to render.

**1. Make the API layer contract-faithful and consider generating it (highest impact).** Model the error envelope as a typed `ApiError { code: ErrorCode; message: string; status: number }` (with `ErrorCode` a union mirrored from `backend/gradient-web/src/error.rs`), have `ApiService` throw that instead of a bare `Error`, and let `errorInterceptor`/components branch on the stable `code` rather than English strings. Then evaluate generating the whole client + DTOs from `docs/gradient-api.yaml` (openapi-typescript or ng-openapi-gen): it eliminates the models/inline split (smell 4), kills the raw-int board enums (smell 5) if the spec is enum-typed, and makes drift a build failure rather than a runtime surprise. Even without full codegen, this is the change that pays back the most.

**2. Collapse to one real-time service and one fetch abstraction.** Delete `BoardLiveService` and fold `/board/live` into `LiveService` (which already has reconnect + backoff); expose typed channel helpers (`liveEval(id)`, `liveBoard()`) that own the socket lifecycle. Pair this with a shared async-fetch primitive - adopt Angular 21 `httpResource()`/`rxResource()`, or a small `loadResource(fetchFn)` returning `{ data, loading, error }` signals - and route the 315 subscribe blocks and 32 `loading` signals through it. This removes the per-component `liveSub`/`unsubscribe`/`setInterval` bookkeeping (smells 6, 7) and standardizes teardown on `takeUntilDestroyed`.

**3. Decompose `evaluation-log.component.ts`.** Extract the log-streaming engine (chunk fetching, virtualized window, `fetch`+`ReadableStream`) into a `LogStreamService`/`LogWindow` that returns signals; move the live wiring onto the unified `LiveService`; route the raw `fetch` log calls through `ApiService` (or a streaming method on it) so they inherit the interceptors; keep the component presentational. Apply the same "component is presentational, service owns IO/state" split to the other >300-line components (`dependency-graph` 631, `job-detail` 480, `integrations` 432, `project-detail` 410, `project-triggers` 377, `live-jobs` 364).

**4. Centralize and strengthen DTO typing.** Give every response type one home (generated, or all under `core/models/`), convert board `kind`/`status` ints to string unions plus a single `jobKindLabel()`/status map, and delete the 15 inline `kind === 1 ? ...` ternaries. Type the trigger/integration `config` as a discriminated union mirrored from the backend to remove the 9 `any` casts (smell 10).

**5. Route `ConfigService` through `ApiService` and make it signal-based.** Drop the duplicated inline envelope and the mutable fields; expose `oidcEnabled`, `createOrg`, etc. as signals so gated UI reacts. While there, migrate `APP_INITIALIZER` to `provideAppInitializer` and reconcile the two `environment*.ts` files (remove the dead runtime-overridden flags).

**6. Backfill tests on the reused core.** Add specs for `ApiService` (envelope unwrap + error mapping, once it carries `code`), `AuthService` (session probe/login/logout), and the CRUD services (`projects`, `evaluations`, `caches`) - the highest-traffic code with the least coverage today.

Bottom line: the frontend is fundamentally sound - all-standalone, fully lazy, strict TS, near-zero `any`, signal-based local state, a clean interceptor/guard/resolver spine, and a disciplined access-directive layer. The concentrated messiness is one god-component (`evaluation-log`), two divergent WebSocket services, an API layer that drops the backend's error `code`, DTOs with no single home, and the missing async-resource abstraction that leaves 315 subscribe blocks hand-rolled. Fixing the API layer and the fetch/real-time abstractions (recs 1-2) removes most of the repetition and the contract drift in one pass.

---

## Related

- AUDIT-WEB.md documents the server-side API, the `{error, code, message}` error envelope + stable `ErrorCode` slugs, and the auth/access model the frontend consumes.
