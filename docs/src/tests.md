# Tests

This page tracks notable tests added to Gradient and where they live.

## Proto handshake — organization peer filtering

Helper `filter_org_peers_without_cache` runs during the `/proto` handshake's
`perform_auth` step. After token validation, each authorized peer that is an
organization is checked against the `organization_cache` table. Organizations
without a subscribed cache are moved into `failed_peers` with reason
`"organization has no cache subscribed"`. If the authorized peer set ends up
empty the connection is rejected with `401 no valid peer tokens provided`.

Backend tests (in `backend/core/src/proto/handler/auth.rs`):

- `proto::handler::auth::tests::filter_org_peers_passes_through_org_with_cache`
- `proto::handler::auth::tests::filter_org_peers_demotes_org_without_cache`
- `proto::handler::auth::tests::filter_org_peers_passes_through_non_org_uuids`
- `proto::handler::auth::tests::filter_org_peers_mixed`
- `proto::handler::auth::tests::validate_then_filter_demotes_org_without_cache`

## Frontend — workers page no-cache banner

When the active organization has no subscribed cache, the workers page shows
a banner instructing the admin to subscribe to a cache before workers can run.

- `WorkersComponent — no-cache banner` — banner show/hide specs at
  `frontend/src/app/features/organizations/workers/workers.component.spec.ts`
