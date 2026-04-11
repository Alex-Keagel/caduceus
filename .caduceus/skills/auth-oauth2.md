---
name: auth-oauth2
version: "1.0"
description: OAuth 2.0 and OIDC flows — authorization code + PKCE, client credentials, and token validation
categories: [auth, security, backend]
triggers: ["oauth2 authorization code pkce", "oidc openid connect", "oauth2 client credentials m2m", "sso oauth integration", "oauth2 token validation"]
tools: [read_file, edit_file, run_tests, shell]
---

# OAuth 2.0 / OIDC Skill

## Flow Selection Guide
| Grant Type | Use Case |
|-----------|---------|
| Authorization Code + PKCE | Web apps, mobile apps — interactive user login |
| Client Credentials | Machine-to-machine (M2M); no user context |
| Device Code | CLI tools, IoT / TV apps without a browser |
| Implicit | **Deprecated** — do not use for new implementations |

## Authorization Code + PKCE (Python / FastAPI)
```bash
pip install authlib itsdangerous
```
```python
from authlib.integrations.starlette_client import OAuth
from fastapi import Request
from fastapi.responses import RedirectResponse

oauth = OAuth()
oauth.register(
    name="provider",
    client_id=settings.CLIENT_ID,
    client_secret=settings.CLIENT_SECRET,
    server_metadata_url="https://provider.example.com/.well-known/openid-configuration",
    client_kwargs={"scope": "openid email profile"},
)

@router.get("/login")
async def login(request: Request):
    redirect_uri = request.url_for("callback")
    return await oauth.provider.authorize_redirect(request, redirect_uri)

@router.get("/callback")
async def callback(request: Request):
    token = await oauth.provider.authorize_access_token(request)
    user_info = token.get("userinfo")   # validated OIDC claims
    # Issue application session or JWT
    return RedirectResponse(url="/dashboard")
```

## Client Credentials (M2M)
```python
import httpx

async def get_m2m_token() -> str:
    async with httpx.AsyncClient() as client:
        r = await client.post(TOKEN_URL, data={
            "grant_type": "client_credentials",
            "client_id": CLIENT_ID,
            "client_secret": CLIENT_SECRET,
            "scope": "api:read api:write",
        })
        r.raise_for_status()
        return r.json()["access_token"]
```
Cache the token until `expires_in` seconds; refresh proactively (30s before expiry).

## Token Validation at Resource Server
```python
from jose import jwt, JWTError

def verify_access_token(token: str) -> dict:
    try:
        payload = jwt.decode(
            token, jwks,           # JWKS fetched from /.well-known/jwks.json
            algorithms=["RS256"],
            audience=settings.API_AUDIENCE,
        )
        return payload
    except JWTError:
        raise HTTPException(status_code=401, detail="Invalid or expired token")
```

## Security Checklist
- Always use PKCE for public clients — never ship a client secret in frontend or mobile code
- Validate `iss`, `aud`, `exp`, and `nbf` claims on every token at the resource server
- Store tokens in memory or `httpOnly`/`Secure`/`SameSite=Strict` cookies — never localStorage
- Implement refresh token rotation; revoke all tokens on logout or password change
- Cache JWKS with a short TTL; fetch fresh keys on validation failure (key rotation support)
