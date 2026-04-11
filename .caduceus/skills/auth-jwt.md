---
name: auth-jwt
version: "1.0"
description: JWT authentication — issuing access/refresh tokens, validation middleware, and password hashing
categories: [auth, security, backend]
triggers: ["jwt token issue", "jwt validation middleware", "refresh token rotation", "jwt bearer auth", "password hashing bcrypt"]
tools: [read_file, edit_file, run_tests, shell]
---

# JWT Authentication Skill

## Dependencies
```bash
pip install python-jose[cryptography] passlib[bcrypt]  # Python
npm install jsonwebtoken bcryptjs @types/jsonwebtoken   # Node.js
```

## Token Issuance (Python)
```python
from jose import jwt
from datetime import datetime, timedelta, timezone

SECRET_KEY = settings.jwt_secret      # min 256-bit random value; load from env
ALGORITHM = "HS256"                    # use RS256 for multi-service architectures

def create_access_token(user_id: str) -> str:
    now = datetime.now(timezone.utc)
    return jwt.encode({
        "sub": user_id,
        "iat": now,
        "exp": now + timedelta(minutes=15),
        "type": "access",
    }, SECRET_KEY, algorithm=ALGORITHM)

def create_refresh_token(user_id: str, jti: str) -> str:
    now = datetime.now(timezone.utc)
    return jwt.encode({
        "sub": user_id,
        "jti": jti,          # unique token ID stored in allowlist
        "iat": now,
        "exp": now + timedelta(days=7),
        "type": "refresh",
    }, SECRET_KEY, algorithm=ALGORITHM)
```

## Validation Middleware (FastAPI)
```python
from fastapi import Depends, HTTPException
from fastapi.security import OAuth2PasswordBearer
from jose import JWTError

oauth2_scheme = OAuth2PasswordBearer(tokenUrl="/auth/token")

async def require_auth(token: str = Depends(oauth2_scheme)) -> str:
    try:
        payload = jwt.decode(token, SECRET_KEY, algorithms=[ALGORITHM])
        if payload.get("type") != "access":
            raise HTTPException(status_code=401, detail="Wrong token type")
        return payload["sub"]
    except JWTError:
        raise HTTPException(status_code=401, detail="Invalid or expired token")
```

## Refresh Token Rotation Flow
1. Client sends refresh token to `POST /auth/refresh`
2. Server decodes and validates `jti` against DB/Redis allowlist
3. Server issues **new** access token + new refresh token (new `jti`)
4. Server deletes old `jti` from the allowlist
5. Return both tokens; client replaces stored tokens

## Password Hashing
```python
from passlib.context import CryptContext

pwd_context = CryptContext(schemes=["bcrypt"], deprecated="auto")

hashed = pwd_context.hash(plain_password)           # hash on registration
is_valid = pwd_context.verify(plain_password, hashed)  # verify on login
```

## Security Rules
- Use RS256 (asymmetric) when tokens are validated by multiple independent services
- JWT payload is base64-encoded, **not encrypted** — never store PII or secrets in claims
- Keep access token TTL short (≤ 15 min); use refresh tokens to maintain session UX
- Store refresh tokens only in `httpOnly`, `Secure`, `SameSite=Strict` cookies
- On password change or explicit logout, invalidate all active refresh tokens for the user
