---
name: ts-zod-validation
version: "1.0"
description: Runtime type validation and schema-derived TypeScript types with Zod
categories: [typescript, validation, backend]
triggers: ["zod schema typescript", "zod parse safeParse", "zod validation runtime", "zod infer type", "zod discriminated union"]
tools: [read_file, edit_file, run_tests, shell]
---

# Zod Validation Skill

## Installation
```bash
npm install zod
```

## Schema Definition
```ts
import { z } from "zod";

const UserSchema = z.object({
  id: z.string().cuid(),
  email: z.string().email(),
  age: z.number().int().min(0).max(150),
  role: z.enum(["admin", "user", "guest"]),
  createdAt: z.coerce.date(),
  tags: z.array(z.string()).default([]),
  metadata: z.record(z.string(), z.unknown()).optional(),
});

type User = z.infer<typeof UserSchema>;
```

## Parsing
```ts
// Throws ZodError on invalid input
const user = UserSchema.parse(rawInput);

// Returns { success, data } or { success: false, error }
const result = UserSchema.safeParse(rawInput);
if (!result.success) {
  const fieldErrors = result.error.flatten().fieldErrors;
  return res.status(400).json({ errors: fieldErrors });
}
const user = result.data;
```

## Transformations
```ts
const TrimmedString = z.string().trim().min(1);
const ISODate = z.string().datetime().transform(s => new Date(s));
const NumericId = z.coerce.number().int().positive();
```

## Discriminated Unions
```ts
const EventSchema = z.discriminatedUnion("type", [
  z.object({ type: z.literal("created"), id: z.string() }),
  z.object({ type: z.literal("deleted"), id: z.string(), reason: z.string() }),
]);
```

## API Validation Middleware (Express)
```ts
function validate<T>(schema: z.ZodSchema<T>) {
  return (req: Request, res: Response, next: NextFunction) => {
    const result = schema.safeParse(req.body);
    if (!result.success) return res.status(400).json({ errors: result.error.flatten() });
    req.body = result.data;
    next();
  };
}
```

## React Hook Form Integration
```ts
import { zodResolver } from "@hookform/resolvers/zod";
const { register, handleSubmit, formState: { errors } } =
  useForm({ resolver: zodResolver(UserSchema) });
```

## Shared Primitives
- Define reusable atoms in `lib/schemas.ts` and import them across feature schemas
- Use `.brand<"UserId">()` for nominal typing on ID strings to prevent mixing types
- Use `z.lazy()` for recursive schemas such as tree structures or nested categories
- Use `z.preprocess()` to coerce/clean data before validation (e.g., trim + lowercase email)
