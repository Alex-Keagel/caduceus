---
name: ts-express-api
version: "1.0"
description: Express.js REST API patterns with TypeScript — routing, middleware, validation, and error handling
categories: [typescript, backend, api]
triggers: ["express api typescript", "express router pattern", "express middleware setup", "express rest backend", "express error handler"]
tools: [read_file, edit_file, run_tests, shell]
---

# Express.js TypeScript API Skill

## Setup
```bash
npm install express helmet cors express-rate-limit
npm install -D typescript @types/express @types/node ts-node-dev
```

## Project Structure
```
src/
  app.ts          # Express app factory (no listen — testable in isolation)
  server.ts       # listen() entry point
  routes/         # Router modules per resource
  controllers/    # Request handler functions
  middleware/     # Custom middleware (auth, validate, error)
  services/       # Business logic layer
```

## App Bootstrap (`app.ts`)
```ts
import express from "express";
import helmet from "helmet";
import cors from "cors";
import { errorHandler } from "./middleware/errorHandler";

const app = express();
app.use(helmet());
app.use(cors({ origin: process.env.ALLOWED_ORIGINS?.split(",") }));
app.use(express.json({ limit: "1mb" }));
app.use("/api/v1/users", userRouter);
app.use(errorHandler);  // Must be last
export default app;
```

## Async Error Wrapper
```ts
type Handler = (req: Request, res: Response, next: NextFunction) => Promise<void>;
const wrapAsync = (fn: Handler) =>
  (req: Request, res: Response, next: NextFunction) =>
    fn(req, res, next).catch(next);
```

## Centralized Error Handler
```ts
class AppError extends Error {
  constructor(public statusCode: number, message: string) {
    super(message);
  }
}

function errorHandler(err: Error, req: Request, res: Response, _next: NextFunction) {
  const status = err instanceof AppError ? err.statusCode : 500;
  const message = status === 500 ? "Internal Server Error" : err.message;
  res.status(status).json({ error: message });
}
```

## Router Pattern
```ts
// routes/users.ts
const router = express.Router();
router.get("/", wrapAsync(UserController.list));
router.post("/", validate(CreateUserSchema), wrapAsync(UserController.create));
router.get("/:id", wrapAsync(UserController.getById));
export default router;
```

## Testing with Supertest
```ts
import request from "supertest";
import app from "../app";

test("POST /api/v1/users returns 201", async () => {
  const res = await request(app)
    .post("/api/v1/users")
    .send({ email: "a@b.com", name: "Alice" });
  expect(res.status).toBe(201);
  expect(res.body).toHaveProperty("id");
});
```

## Security Checklist
- `helmet()` sets secure HTTP headers (HSTS, CSP, X-Frame-Options, etc.)
- Apply `express-rate-limit` on auth and sensitive endpoints
- Validate all request body input with Zod before touching business logic
- Never return stack traces or internal error messages to clients in production
