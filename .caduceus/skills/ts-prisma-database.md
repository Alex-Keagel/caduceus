---
name: ts-prisma-database
version: "1.0"
description: Type-safe database access with Prisma ORM — schema design, migrations, queries, and testing
categories: [typescript, database, backend]
triggers: ["prisma orm typescript", "prisma schema define", "prisma migrate dev", "prisma client query", "prisma database access"]
tools: [read_file, edit_file, shell, run_tests]
---

# Prisma ORM Skill

## Setup
```bash
npm install prisma @prisma/client
npx prisma init --datasource-provider postgresql
```

## Schema Definition (`prisma/schema.prisma`)
```prisma
generator client {
  provider = "prisma-client-js"
}

datasource db {
  provider = "postgresql"
  url      = env("DATABASE_URL")
}

model User {
  id        String   @id @default(cuid())
  email     String   @unique
  name      String?
  posts     Post[]
  createdAt DateTime @default(now())
  updatedAt DateTime @updatedAt
}

model Post {
  id        String  @id @default(cuid())
  title     String
  published Boolean @default(false)
  author    User    @relation(fields: [authorId], references: [id], onDelete: Cascade)
  authorId  String
  @@index([authorId])
}
```

## Migrations
```bash
npx prisma migrate dev --name add_user_table   # dev: apply + regenerate client
npx prisma migrate deploy                       # prod: apply only, no client regen
npx prisma db push                              # prototyping: no migration file created
```

## Singleton Client Pattern
```ts
// lib/prisma.ts
import { PrismaClient } from "@prisma/client";
const globalForPrisma = globalThis as unknown as { prisma?: PrismaClient };
export const prisma = globalForPrisma.prisma ?? new PrismaClient();
if (process.env.NODE_ENV !== "production") globalForPrisma.prisma = prisma;
```

## Common Query Patterns
```ts
// Create
const user = await prisma.user.create({ data: { email, name } });

// Find with nested relation
const posts = await prisma.post.findMany({
  where: { author: { email } },
  include: { author: { select: { name: true } } },
  orderBy: { createdAt: "desc" },
  take: 10,
  skip: page * 10,
});

// Upsert
await prisma.user.upsert({
  where: { email },
  update: { name },
  create: { email, name },
});

// Sequential transaction
await prisma.$transaction([
  prisma.post.update({ where: { id }, data: { published: true } }),
  prisma.user.update({ where: { id: authorId }, data: { publishedCount: { increment: 1 } } }),
]);
```

## Testing
- Point tests at `TEST_DATABASE_URL` (separate test database)
- Use `prisma.$executeRawUnsafe("TRUNCATE ...")` in `beforeEach` to reset state
- Mock `prisma` with `jest-mock-extended` for unit tests that skip the database
