# Privacy

Gradient instances send no data to Wavelens GmbH or any third party. There is no telemetry, no usage tracking and no phone-home.

## Self-Hosted Data

All data remain on infrastructure of the operator.

| Data | Location |
|---|---|
| Accounts, projects, evaluations, build logs | PostgreSQL of the instance |
| Build outputs and cache contents | Disk or S3 storage of the operator |
| Store paths on workers | Nix store of each worker |

- The operator is the data controller under the GDPR.
- Users can delete their own account in the web UI or through `DELETE /user`.

## Outgoing Connections

Gradient can only reach services the operator configured.

- Git hosts, upstream caches and S3 storage.
- Webhooks, OIDC providers and SCIM clients.
- Crash reports to Wavelens, only with `services.gradient.sentry.enable = true`. Off by default.

## Diagnostic Reports

A [diagnostic report](https://wavelens.github.io/gradient/guides/diagnostic-report/) can leave the instance only as an attachment a user uploaded. Identities stay anonymized without an explicit opt-in. Credentials are never part of the file.
