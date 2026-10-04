# Monitor Gradient

Server metrics in Prometheus or any OpenTelemetry collector: workers, queue, build and evaluation counts, cache size and HTTP traffic. The [Job Board](../ui/job-board.md) can show the same data inside Gradient. This guide is for feeding existing dashboards and alerts.

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

1.  Any random string, e.g. `openssl rand -base64 32`. `GET /metrics` will answer `404` without the file.
2.  The bundled reverse proxy does not forward `/metrics`. The scraper must talk to `listenAddr` and `port` directly.

The endpoint rate limit will refill one request per second, with a burst of 5.

## 2. OpenTelemetry

```nix
services.gradient.metrics.otlp = {
  endpoint = "http://collector.example.com:4318/v1/metrics"; # (1)!
  pushIntervalSecs = 30;
};
```

1.  OTLP over HTTP with protobuf, including the `/v1/metrics` path. The service name is `gradient`.

## Verify Deployment

- Prometheus: the `gradient` target will show **UP**. `gradient_workers_connected` will return the number of connected workers.
- OpenTelemetry: the server log will show `OTLP metric push enabled`. The collector will receive `gradient_jobs_pending`.

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

- [Job Board](../ui/job-board.md): scheduler scores, worker load and build cost
- [Configuration](../reference/configuration.md#metrics): retention and sampling intervals
