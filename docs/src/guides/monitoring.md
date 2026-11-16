<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Monitor Gradient

Gradient reports server metrics that Prometheus or any OpenTelemetry collector can consume. The metrics cover workers, the queue, build and evaluation counts, cache size and HTTP traffic. The [Job Board](../ui/job-board.md) can display the same data inside Gradient. Use the setup below to feed dashboards and alerts that already exist.

**Requirements:**

- A running instance, see [Quick Start](../get-started/quick-start.md)
- Prometheus or an OpenTelemetry collector

| | Prometheus | OpenTelemetry |
|---|---|---|
| Direction | Prometheus scraping `GET /metrics` | Gradient pushing OTLP over HTTP |
| Metrics | All metrics below | Workers, jobs and cache gauges |
| Pick when | Prometheus is already active | A collector is already active, or the server is unreachable for a scraper |

## 1. Prometheus

```nix
services.gradient.metrics.tokenFile = "/run/secrets/gradient-metrics"; # (1)!

services.prometheus.scrapeConfigs = [{
  job_name = "gradient";
  authorization.credentials_file = "/run/secrets/gradient-metrics";
  static_configs = [{ targets = [ "127.0.0.1:3000" ]; }]; # (2)!
}];
```

1. Generate a random secret, for example with `openssl rand -base64 32`. Without this file, a `GET /metrics` request can receive a `404` response.
2. The bundled reverse proxy does not forward `/metrics`. The scraper must connect to `listenAddr` and `port` directly.

The endpoint rate limit will refill one request per second, with a burst of 5.

## 2. OpenTelemetry

```nix
services.gradient.metrics.otlp = {
  endpoint = "http://collector.example.com:4318/v1/metrics"; # (1)!
  pushIntervalSecs = 30;
  tracesEndpoint = "http://collector.example.com:4318/v1/traces"; # (2)!
  serviceName = "gradient"; # (3)!
  tracesSampleRatio = 1.0; # (4)!
};
```

1. The endpoint protocol is OTLP over HTTP, with protobuf encoding. The path needs `/v1/metrics`. Metrics report the service name set in `serviceName`.
2. Always include the path `/v1/traces` in the URL. The server can send spans to the exact URL. A URL without this path can lose all spans with no error. The server can export its own spans to that URL. These spans capture the duration of a step on the server. Examples include transactions from the graph writer and the handling of worker messages. Spans for dependencies such as `hyper` stay out of the export.
3. All instances report with the same service name. The collector can then show spans from every instance as one service.
4. The fraction of traces to export. From `0.0` (none) to `1.0` (all). A heavily loaded instance can lower the fraction to `0.1`, for example, to reduce the number of spans.

## Verify Deployment

- Prometheus: confirm the `gradient` target shows **UP**. The `gradient_workers_connected` metric reports the count of connected workers.
- OpenTelemetry: check the server log for `OTLP metric push enabled`. Collectors receive `gradient_jobs_pending` afterward.
- OpenTelemetry traces: check the server log for `OTLP span export enabled`. The collector can then receive spans such as `transact_once`.

## Metrics

| Metric | Prometheus | OTLP | Meaning |
|---|---|---|---|
| `gradient_workers_connected` | yes | yes | Connected workers |
| `gradient_jobs_pending`, `gradient_jobs_active` | yes | yes | Jobs waiting and running |
| `gradient_cache_bytes`, `gradient_cache_nar_bytes`, `gradient_cache_packages` | yes | yes | Cache size and path count |
| `gradient_builds_total{status}`, `gradient_builds_in_state{status}` | yes | | Builds by status |
| `gradient_evaluations_total{status}`, `gradient_evaluations_in_state{status}` | yes | | Evaluations by status |
| `gradient_cache_nar_requests_total`, `gradient_cache_nar_bytes_sent_total` | yes | | Cache traffic |
| `gradient_http_requests_total`, `gradient_http_request_duration_seconds` | yes | | HTTP traffic |
| `gradient_upload_*` | yes | | Upload admission: in flight, queue depth, grants, wait time |
| `gradient_info`, `gradient_uptime_seconds` | yes | | Version and uptime |

## Alerts

```yaml
# prometheus rules
groups:
  - name: gradient
    rules:
      - alert: GradientNoWorkers
        expr: gradient_workers_connected == 0
      - alert: GradientQueueStuck
        expr: gradient_jobs_pending > 100 and gradient_jobs_active == 0
      - alert: GradientBuildsFailing
        expr: increase(gradient_builds_total{status=~"FailedPermanent|FailedTimeout"}[1h]) > 10
```

## Next Steps

- [Job Board](../ui/job-board.md) shows scheduler scores, worker load and build cost.
- The [Configuration](../reference/configuration.md#metrics) reference covers retention and sampling intervals.
