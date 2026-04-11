---
name: ts-nextjs-app
version: "1.0"
description: Build full-stack Next.js 15 applications with App Router, Server Components, and server actions
categories: [typescript, frontend, nextjs]
triggers: ["nextjs app router", "next.js 15 project", "server component nextjs", "next.js server action", "nextjs full stack"]
tools: [read_file, edit_file, run_tests, shell]
---

# Next.js 15 App Router Skill

## Bootstrap
```bash
npx create-next-app@latest my-app --typescript --tailwind --eslint --app --src-dir
```

## Directory Layout
```
src/
  app/
    layout.tsx          # Root layout — wraps all pages
    page.tsx            # Home route component
    (auth)/             # Route group — no URL segment added
    api/orders/route.ts # API route handler
  components/           # Shared UI components
  lib/                  # Server-only utilities (DB, auth)
  actions/              # Server actions
```

## Server vs Client Components
- All components are Server Components by default — zero JS shipped to client
- Add `"use client"` only for interactivity, browser APIs, or React hooks
- Pass serializable props from Server to Client; avoid fetching on the client
- Mark server-only modules with `import "server-only"` to prevent accidental imports

## Data Fetching in Server Components
```tsx
async function ProductList() {
  const products = await db.product.findMany();
  return <ul>{products.map(p => <li key={p.id}>{p.name}</li>)}</ul>;
}
```
- Use `cache()` from React for request deduplication within a render
- Tag fetches: `{ next: { tags: ["products"] } }` for granular revalidation
- Call `revalidateTag("products")` or `revalidatePath("/shop")` from server actions

## Server Actions
```tsx
"use server";
export async function addToCart(formData: FormData) {
  const id = formData.get("productId") as string;
  await db.cart.upsert({ where: { id }, create: { id }, update: {} });
  revalidatePath("/cart");
}
```

## Dynamic Metadata
```tsx
export async function generateMetadata({ params }): Promise<Metadata> {
  const product = await getProduct(params.id);
  return { title: product.name, description: product.description };
}
```

## Performance
- `next/image` — automatic WebP/AVIF optimization; always set `width` and `height`
- `next/font` — zero-CLS font loading with CSS variable injection
- Analyze bundle size: `ANALYZE=true next build`

## Testing
```bash
pnpm add -D vitest @vitejs/plugin-react @testing-library/react playwright
```
Use Playwright for e2e flows; configure `next/jest` for unit and component tests.
