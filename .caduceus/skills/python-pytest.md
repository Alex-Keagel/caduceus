---
name: python-pytest
version: "1.0"
description: Python testing with pytest — fixtures, parametrize, mocking, async tests, and coverage reporting
categories: [python, testing, quality]
triggers: ["pytest fixtures", "pytest parametrize", "pytest mock mocker", "pytest async test", "python test coverage"]
tools: [read_file, edit_file, run_tests, shell]
---

# Pytest Testing Skill

## Setup
```bash
pip install pytest pytest-cov pytest-asyncio pytest-mock httpx factory-boy
```

`pyproject.toml`:
```toml
[tool.pytest.ini_options]
asyncio_mode = "auto"
testpaths = ["tests"]
addopts = "--tb=short --strict-markers -q"
```

## Fixture Patterns
```python
import pytest

@pytest.fixture
def db(tmp_path):
    engine = create_engine(f"sqlite:///{tmp_path}/test.db")
    Base.metadata.create_all(engine)
    yield engine
    engine.dispose()

@pytest.fixture
async def client(app):
    from httpx import AsyncClient
    async with AsyncClient(app=app, base_url="http://test") as c:
        yield c
```
- `scope="session"` — expensive setup (DB migrations, container startup); shared across all tests
- `scope="function"` (default) — full isolation; a new instance per test

## Parametrize
```python
@pytest.mark.parametrize("email,valid", [
    ("user@example.com", True),
    ("not-an-email", False),
    ("", False),
])
def test_email_validation(email, valid):
    assert validate_email(email) == valid
```

## Mocking with pytest-mock
```python
def test_checkout_calls_payment_api(mocker):
    mock = mocker.patch("app.services.payment.charge", return_value={"status": "ok"})
    result = checkout(cart)
    mock.assert_called_once_with(amount=cart.total)
    assert result.status == "ok"
```

## Async Tests
```python
@pytest.mark.asyncio
async def test_async_fetch():
    result = await fetch_user(id=1)
    assert result.id == 1
```

## Test Factories
```python
import factory

class UserFactory(factory.Factory):
    class Meta:
        model = User
    email = factory.Sequence(lambda n: f"user{n}@example.com")
    name = factory.Faker("name")
```

## Coverage
```bash
pytest --cov=app --cov-report=term-missing --cov-fail-under=80
```

## CI Integration
```yaml
- run: pytest --cov=app --cov-report=xml
- uses: codecov/codecov-action@v4
  with:
    files: ./coverage.xml
```

## Test Organization
- Mirror the `app/` structure in `tests/` (e.g., `tests/services/test_user.py`)
- Use `conftest.py` for shared fixtures accessible to all tests in a directory
- Separate unit tests (no I/O) from integration tests that need a live DB or HTTP server
