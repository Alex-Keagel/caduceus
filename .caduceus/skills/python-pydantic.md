---
name: python-pydantic
version: "1.0"
description: Data validation and settings management with Pydantic v2 — models, field validators, and config
categories: [python, validation, data]
triggers: ["pydantic basemodel", "pydantic v2 validate", "pydantic field validator", "pydantic settings env", "pydantic model config"]
tools: [read_file, edit_file, run_tests, shell]
---

# Pydantic v2 Skill

## Installation
```bash
pip install pydantic pydantic-settings email-validator
```

## Basic Model
```python
from pydantic import BaseModel, EmailStr, Field, field_validator, model_validator
from datetime import datetime
from typing import Annotated

class User(BaseModel):
    id: str
    email: EmailStr
    name: Annotated[str, Field(min_length=1, max_length=100)]
    age: Annotated[int, Field(ge=0, le=150)]
    created_at: datetime = Field(default_factory=datetime.utcnow)
    tags: list[str] = []
```

## Field Validators
```python
@field_validator("name")
@classmethod
def name_must_not_be_blank(cls, v: str) -> str:
    if not v.strip():
        raise ValueError("name cannot be blank")
    return v.strip()
```

## Model Validators (cross-field)
```python
@model_validator(mode="after")
def check_admin_has_email(self) -> "User":
    if getattr(self, "role", None) == "admin" and not self.email:
        raise ValueError("admin users must have an email")
    return self
```

## Serialization
```python
user = User(id="1", email="a@b.com", name="Alice", age=30)

user.model_dump()                     # dict
user.model_dump(exclude_none=True)    # omit None values
user.model_dump_json()                # JSON bytes
User.model_validate(raw_dict)         # parse from dict
User.model_validate_json(json_str)    # parse from JSON string
```

## Model Configuration
```python
class User(BaseModel):
    model_config = {
        "str_strip_whitespace": True,   # auto-strip string fields
        "validate_assignment": True,    # validate on attribute set
        "from_attributes": True,        # enables model_validate(orm_object)
    }
```

## Settings Management
```python
from pydantic_settings import BaseSettings, SettingsConfigDict

class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_file=".env", env_prefix="APP_")
    database_url: str
    secret_key: str
    debug: bool = False
    max_connections: int = 10

settings = Settings()  # reads APP_DATABASE_URL, APP_SECRET_KEY, etc.
```

## Custom Annotated Types
```python
from typing import Annotated
from pydantic import Field

PositiveDecimal = Annotated[Decimal, Field(gt=0)]
ShortString = Annotated[str, Field(max_length=255, strip_whitespace=True)]
```

## Testing Validation
```python
def test_blank_name_raises_validation_error():
    with pytest.raises(ValidationError) as exc:
        User(id="1", email="a@b.com", name="   ", age=30)
    assert "name cannot be blank" in str(exc.value)
```
