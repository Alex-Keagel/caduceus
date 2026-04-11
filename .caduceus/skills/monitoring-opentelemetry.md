---
name: monitoring-opentelemetry
version: "1.0"
description: OpenTelemetry instrumentation for distributed traces, metrics, and log correlation across services
categories: [monitoring, observability, tracing]
triggers: ["opentelemetry setup", "otel distributed tracing", "otel sdk python nodejs", "jaeger tempo tracing", "opentelemetry collector config"]
tools: [read_file, edit_file, shell, run_tests]
---

# OpenTelemetry Instrumentation Skill

## Signal Types
- **Traces**: distributed request flow across services; spans linked by trace ID + parent span ID
- **Metrics**: aggregated numeric measurements exported to Prometheus or OTLP backends
- **Logs**: structured events correlated to trace/span IDs for cross-signal analysis

## Python Setup
```bash
pip install opentelemetry-sdk opentelemetry-exporter-otlp \
  opentelemetry-instrumentation-fastapi \
  opentelemetry-instrumentation-sqlalchemy \
  opentelemetry-instrumentation-httpx
```

```python
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter
from opentelemetry.instrumentation.fastapi import FastAPIInstrumentor

provider = TracerProvider()
provider.add_span_processor(
    BatchSpanProcessor(OTLPSpanExporter(endpoint="http://otel-collector:4317"))
)
trace.set_tracer_provider(provider)
FastAPIInstrumentor.instrument_app(app)   # auto-instruments all routes
```

## Manual Span Creation
```python
tracer = trace.get_tracer(__name__)

async def process_order(order_id: str):
    with tracer.start_as_current_span("process_order") as span:
        span.set_attribute("order.id", order_id)
        span.set_attribute("service.component", "order-processor")
        result = await db.get_order(order_id)
        if not result:
            span.set_status(trace.Status(trace.StatusCode.ERROR, "Order not found"))
            span.record_exception(OrderNotFoundError(order_id))
            raise OrderNotFoundError(order_id)
        return result
```

## Node.js / TypeScript Auto-Instrumentation
```bash
npm install @opentelemetry/sdk-node @opentelemetry/auto-instrumentations-node \
  @opentelemetry/exporter-trace-otlp-grpc
```
```ts
// instrument.ts — import BEFORE all other modules
import { NodeSDK } from "@opentelemetry/sdk-node";
import { getNodeAutoInstrumentations } from "@opentelemetry/auto-instrumentations-node";
const sdk = new NodeSDK({ instrumentations: [getNodeAutoInstrumentations()] });
sdk.start();
```

## OTel Collector Configuration
```yaml
receivers:
  otlp:
    protocols:
      grpc: { endpoint: "0.0.0.0:4317" }
      http: { endpoint: "0.0.0.0:4318" }
exporters:
  jaeger:
    endpoint: "jaeger:14250"
    tls: { insecure: true }
  prometheus:
    endpoint: "0.0.0.0:8889"
service:
  pipelines:
    traces: { receivers: [otlp], exporters: [jaeger] }
    metrics: { receivers: [otlp], exporters: [prometheus] }
```

## Best Practices
- Set resource attributes at startup: `service.name`, `service.version`, `deployment.environment`
- Use W3C `traceparent` headers for cross-service propagation (auto-instrumentation handles this)
- Sample high-traffic spans with `ParentBasedTraceIdRatioSampler` to reduce storage cost
- Emit spans with `ERROR` status and call `record_exception()` before re-raising errors
