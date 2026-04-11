---
name: integration-webhook
version: "1.0"
description: Webhook receiver and sender patterns — HMAC signature verification, idempotency, retries, and delivery tracking
categories: [integration, backend, api]
triggers: ["webhook signature verify", "webhook idempotency", "webhook retry delivery", "webhook sender pattern", "webhook receiver handler"]
tools: [read_file, edit_file, run_tests, shell]
---

# Webhook Patterns Skill

## Receiving Webhooks

### HMAC-SHA256 Signature Verification
```python
import hmac
import hashlib
from fastapi import Request, HTTPException

async def verify_webhook_signature(request: Request, secret: str) -> bytes:
    body = await request.body()
    received = request.headers.get("X-Signature-256", "")
    expected = "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    if not hmac.compare_digest(expected, received):
        raise HTTPException(status_code=401, detail="Invalid webhook signature")
    return body
```
Always use `hmac.compare_digest` — never `==` — to prevent timing side-channel attacks.

### Idempotent Handler
```python
@router.post("/webhooks/orders", status_code=200)
async def handle_order_webhook(request: Request):
    body = await verify_webhook_signature(request, settings.webhook_secret)
    event = OrderEvent.model_validate_json(body)

    # Deduplicate by event ID using Redis with 1-hour TTL
    already_seen = await redis.setnx(f"webhook:{event.id}", "1")
    if not already_seen:
        return {"status": "already_processed"}
    await redis.expire(f"webhook:{event.id}", 3600)

    # Enqueue for async processing; return 200 immediately
    await queue.enqueue(process_order_event, event.model_dump())
    return {"status": "accepted"}
```
Return `200` immediately and process asynchronously to prevent timeout-triggered retries.

## Sending Webhooks

### Delivery with Exponential Backoff
```python
import httpx
import json
import hmac
import hashlib
from tenacity import retry, stop_after_attempt, wait_exponential

@retry(stop=stop_after_attempt(5), wait=wait_exponential(multiplier=1, max=60))
async def deliver_webhook(url: str, event: dict, secret: str) -> None:
    body = json.dumps(event, separators=(",", ":"))
    sig = hmac.new(secret.encode(), body.encode(), hashlib.sha256).hexdigest()
    async with httpx.AsyncClient(timeout=10.0) as client:
        r = await client.post(url, content=body, headers={
            "Content-Type": "application/json",
            "X-Signature-256": f"sha256={sig}",
            "X-Event-ID": event["id"],
            "X-Event-Type": event["type"],
        })
        r.raise_for_status()   # 4xx/5xx triggers retry
```

## Standard Event Schema
```json
{
  "id": "evt_01HXYZ123",
  "type": "order.created",
  "created_at": "2024-01-15T10:30:00Z",
  "api_version": "2024-01",
  "data": {
    "order_id": "ord_123",
    "customer_id": "cust_456"
  }
}
```

## Operational Checklist
- Log every delivery attempt: URL, response status, latency, attempt number
- Route to a dead-letter queue after `max_attempts`; alert and allow manual replay
- Expose a delivery history UI or API so subscribers can inspect and retry events
- Rotate signing secrets with a transition window: accept old and new signatures simultaneously
- Validate that your payload fits within the subscriber's stated size limit (often 5–10 MB)
