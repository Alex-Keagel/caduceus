---
name: python-sqlalchemy
version: "1.0"
description: Database access with SQLAlchemy 2.x — ORM models, async sessions, queries, and Alembic migrations
categories: [python, database, backend]
triggers: ["sqlalchemy 2 orm", "sqlalchemy async session", "alembic migrate", "sqlalchemy mapped column", "python orm database access"]
tools: [read_file, edit_file, run_tests, shell]
---

# SQLAlchemy 2.x + Alembic Skill

## Setup
```bash
pip install sqlalchemy alembic asyncpg greenlet   # async PostgreSQL
# or: psycopg2-binary for synchronous usage
alembic init alembic
```

## Model Definition (Mapped annotation style)
```python
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship
from sqlalchemy import String, ForeignKey

class Base(DeclarativeBase):
    pass

class User(Base):
    __tablename__ = "users"
    id: Mapped[int] = mapped_column(primary_key=True)
    email: Mapped[str] = mapped_column(String(255), unique=True, nullable=False)
    name: Mapped[str | None]
    posts: Mapped[list["Post"]] = relationship(back_populates="author")

class Post(Base):
    __tablename__ = "posts"
    id: Mapped[int] = mapped_column(primary_key=True)
    title: Mapped[str]
    author_id: Mapped[int] = mapped_column(ForeignKey("users.id"))
    author: Mapped["User"] = relationship(back_populates="posts")
```

## Async Engine + Session Factory
```python
from sqlalchemy.ext.asyncio import create_async_engine, async_sessionmaker

engine = create_async_engine(DATABASE_URL, echo=False, pool_size=10, max_overflow=20)
AsyncSessionLocal = async_sessionmaker(engine, expire_on_commit=False)

async def get_db():          # FastAPI dependency
    async with AsyncSessionLocal() as session:
        yield session
```

## Query Patterns (2.x `select()` style)
```python
from sqlalchemy import select, update, delete

# Fetch one
result = await session.execute(select(User).where(User.email == email))
user = result.scalar_one_or_none()

# Create
session.add(User(email=email, name=name))
await session.commit()

# Bulk update
await session.execute(update(User).where(User.id == uid).values(name=new_name))
await session.commit()

# Delete
await session.execute(delete(Post).where(Post.author_id == uid))
await session.commit()
```

## Alembic Migrations
Configure `alembic/env.py` with `target_metadata = Base.metadata` and async engine.
```bash
alembic revision --autogenerate -m "add_users_table"
alembic upgrade head
alembic downgrade -1
alembic history --verbose
```

## Testing
- Use `sqlite+aiosqlite:///` or a dedicated test PostgreSQL database via env var
- Call `Base.metadata.create_all(engine)` in setup; `drop_all` in teardown
- Wrap each test in a savepoint that rolls back: `await session.begin_nested()`
- Never share sessions between tests — create a fresh session per test function
