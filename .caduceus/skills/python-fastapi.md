---
name: python-fastapi
version: "1.0"
description: FastAPI application patterns — routers, dependency injection, async endpoints, and OpenAPI docs
categories: [python, backend, api]
triggers: ["fastapi app setup", "fastapi router", "fastapi dependency injection", "fastapi async endpoint", "python fastapi project"]
tools: [read_file, edit_file, run_tests, shell]
---

# FastAPI Application Skill

## Setup
```bash
pip install fastapi uvicorn[standard] pydantic-settings
```

## Project Structure
```
app/
  main.py           # FastAPI() instance, lifespan, middleware registration
  routers/          # APIRouter modules per resource
  models/           # Pydantic request/response models
  services/         # Business logic (no FastAPI imports)
  dependencies/     # Shared Depends() factories
  core/config.py    # Settings via pydantic-settings
```

## App Bootstrap with Lifespan
```python
from contextlib import asynccontextmanager
from fastapi import FastAPI

@asynccontextmanager
async def lifespan(app: FastAPI):
    await db.connect()      # startup
    yield
    await db.disconnect()   # shutdown

app = FastAPI(title="My API", version="1.0", lifespan=lifespan)
app.include_router(users.router, prefix="/api/v1/users", tags=["users"])
```

## Request/Response Models
```python
from pydantic import BaseModel, EmailStr, field_validator

class UserCreate(BaseModel):
    email: EmailStr
    name: str

    @field_validator("name")
    @classmethod
    def name_not_empty(cls, v: str) -> str:
        if not v.strip():
            raise ValueError("name cannot be blank")
        return v.strip()

class UserResponse(BaseModel):
    id: str
    email: str
    name: str
    model_config = {"from_attributes": True}
```

## Dependency Injection
```python
async def get_current_user(token: str = Depends(oauth2_scheme)) -> User:
    payload = decode_jwt(token)
    return await user_repo.get(payload["sub"])

@router.get("/me", response_model=UserResponse)
async def get_me(user: User = Depends(get_current_user)):
    return user
```

## Background Tasks
```python
@router.post("/emails", status_code=202)
async def send_email(payload: EmailPayload, tasks: BackgroundTasks):
    tasks.add_task(mailer.send, payload.to, payload.subject, payload.body)
    return {"status": "queued"}
```

## Error Handling
```python
from fastapi import HTTPException
raise HTTPException(status_code=404, detail="User not found")
```
Use `@app.exception_handler(RequestValidationError)` for a custom validation error response format.

## Testing
```python
from fastapi.testclient import TestClient
client = TestClient(app)

def test_create_user():
    r = client.post("/api/v1/users", json={"email": "a@b.com", "name": "Alice"})
    assert r.status_code == 201
    assert r.json()["email"] == "a@b.com"
```
