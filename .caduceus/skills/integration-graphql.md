---
name: integration-graphql
version: "1.0"
description: GraphQL API design — schema-first SDL, resolvers, DataLoader N+1 prevention, pagination, and authorization
categories: [api, integration, backend]
triggers: ["graphql schema design", "graphql resolver python", "graphql dataloader n+1", "graphql pagination relay", "graphql authorization"]
tools: [read_file, edit_file, run_tests, shell]
---

# GraphQL API Design Skill

## Schema-First Development
Write the SDL (Schema Definition Language) before any resolver code.
Types and fields define the public contract; implementation follows.

```graphql
type User {
  id: ID!
  email: String!
  posts(first: Int = 10, after: String): PostConnection!
}

type PostConnection {
  edges: [PostEdge!]!
  pageInfo: PageInfo!
  totalCount: Int!
}

type PageInfo {
  hasNextPage: Boolean!
  endCursor: String
}

type Query {
  user(id: ID!): User
  posts(filter: PostFilter): PostConnection!
}

type Mutation {
  createPost(input: CreatePostInput!): Post!
  publishPost(id: ID!): Post!
}
```

## Python — Strawberry Framework
```bash
pip install strawberry-graphql[fastapi]
```
```python
import strawberry
from strawberry.fastapi import GraphQLRouter

@strawberry.type
class Query:
    @strawberry.field
    async def user(self, id: strawberry.ID, info: strawberry.types.Info) -> "User | None":
        return await info.context["loaders"].user.load(id)

schema = strawberry.Schema(query=Query, mutation=Mutation)
graphql_app = GraphQLRouter(schema, context_getter=get_context)
app.include_router(graphql_app, prefix="/graphql")
```

## DataLoader — Prevent N+1 Queries
```python
from strawberry.dataloader import DataLoader

async def batch_load_users(keys: list[str]) -> list["User | None"]:
    users_by_id = {u.id: u for u in await db.fetch_users_by_ids(keys)}
    return [users_by_id.get(k) for k in keys]

user_loader = DataLoader(load_fn=batch_load_users)
```
Always use DataLoader for relation fields (author, user, organization).
One DataLoader per request context — not a singleton.

## Relay Cursor Pagination
- Return `Connection` types: `edges`, `pageInfo` (`hasNextPage`, `endCursor`), `totalCount`
- Accept `first` + `after` for forward pagination; `last` + `before` for backward
- Encode cursors as opaque strings: `base64(typename:pk)` or offset-based

## Error Handling
- Return `null` + `errors` array for expected failures (not found, access denied)
- Add `extensions` to errors: `{ "code": "NOT_FOUND", "path": ["user"] }`
- Do not expose stack traces or internal error messages in the `errors` array

## Security
- Limit query depth and complexity to prevent DoS (`strawberry.extensions.MaxAliasesLimiter`)
- Disable introspection in production for APIs not intended to be public
- Apply field-level authorization inside resolvers using `info.context["current_user"]`
- Rate-limit by IP and by authenticated user separately
