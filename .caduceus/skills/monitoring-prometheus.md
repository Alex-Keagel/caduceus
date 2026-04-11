---
name: monitoring-prometheus
version: "1.0"
description: Prometheus metrics instrumentation and Grafana dashboard patterns for application observability
categories: [monitoring, observability, devops]
triggers: ["prometheus metrics setup", "grafana dashboard queries", "prometheus counter histogram gauge", "scrape metrics endpoint", "alert prometheus"]
tools: [read_file, edit_file, shell, run_tests]
---

# Prometheus + Grafana Skill

## Local Stack (Docker Compose)
```yaml
services:
  prometheus:
    image: prom/prometheus
    ports: ["9090:9090"]
    volumes: ["./prometheus.yml:/etc/prometheus/prometheus.yml"]
  grafana:
    image: grafana/grafana
    ports: ["3000:3000"]
    environment:
      GF_AUTH_ANONYMOUS_ENABLED: "true"
      GF_AUTH_ANONYMOUS_ORG_ROLE: Admin
```

`prometheus.yml`:
```yaml
global:
  scrape_interval: 15s
scrape_configs:
  - job_name: "my-app"
    static_configs:
      - targets: ["host.docker.internal:8080"]
    metrics_path: /metrics
```

## Python Instrumentation
```bash
pip install prometheus-client
```
```python
from prometheus_client import Counter, Histogram, Gauge, start_http_server

REQUEST_COUNT = Counter(
    "http_requests_total",
    "Total HTTP requests",
    ["method", "endpoint", "status"],
)
REQUEST_LATENCY = Histogram(
    "http_request_duration_seconds",
    "HTTP request latency in seconds",
    ["method", "endpoint"],
    buckets=[.005, .01, .025, .05, .1, .25, .5, 1, 2.5],
)
ACTIVE_CONNECTIONS = Gauge("active_connections", "Active WebSocket connections")

# Usage in handlers
REQUEST_COUNT.labels(method="GET", endpoint="/users", status="200").inc()
with REQUEST_LATENCY.labels(method="POST", endpoint="/orders").time():
    result = process_order()

start_http_server(8080)   # expose /metrics on port 8080
```

## Metric Types
| Type | Behavior | Use Case |
|------|----------|---------|
| `Counter` | Monotonically increasing | Request count, error count |
| `Gauge` | Up and down | Queue depth, active connections, memory |
| `Histogram` | Sampled with configurable buckets | Latency, request sizes |
| `Summary` | Client-side quantiles | Avoid for high-cardinality labels |

## Essential Metrics to Expose
- `http_requests_total{method, endpoint, status}` — request rate and error rate
- `http_request_duration_seconds{method, endpoint}` — latency percentiles
- `db_query_duration_seconds{query_type}` — database performance per query type
- `queue_depth{queue_name}` — backlog size; alert on sustained high values
- Standard process metrics: `process_resident_memory_bytes`, `process_cpu_seconds_total`

## Grafana Query Examples
```promql
# Per-second request rate
rate(http_requests_total[5m])

# 5xx error rate percentage
100 * rate(http_requests_total{status=~"5.."}[5m])
  / rate(http_requests_total[5m])

# P95 request latency
histogram_quantile(0.95, rate(http_request_duration_seconds_bucket[5m]))
```

## Alerting Rules
Alert when error rate exceeds 1% or P99 latency exceeds SLA for 5 consecutive minutes.
Expose `/metrics` on an internal-only port — do not expose to the public internet.
