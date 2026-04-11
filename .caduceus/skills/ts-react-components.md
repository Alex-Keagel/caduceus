---
name: ts-react-components
version: "1.0"
description: React 19 component patterns — hooks, context, Suspense, error boundaries, and performance
categories: [typescript, frontend, react]
triggers: ["react component patterns", "react hooks typescript", "react context provider", "react suspense boundary", "react performance memo"]
tools: [read_file, edit_file, run_tests, shell]
---

# React Component Patterns Skill

## Component Design Principles
- Prefer function components; never use class components for new code
- Keep components under ~150 lines; extract when they grow larger
- Keep state as low in the tree as possible before lifting it up

## Custom Hooks
```tsx
function useUser(id: string) {
  const [state, setState] = useState<{
    data?: User; error?: Error; loading: boolean;
  }>({ loading: true });

  useEffect(() => {
    fetchUser(id)
      .then(data => setState({ data, loading: false }))
      .catch(error => setState({ error, loading: false }));
  }, [id]);

  return state;
}
```
- Name custom hooks `use*`; they can call other hooks freely
- Always specify the full dependency array for `useEffect` — never suppress the lint rule
- Use `useCallback` for stable event handlers passed as props to memoized children
- Use `useMemo` for expensive derived computations, not as a default optimization

## Context Pattern
```tsx
const ThemeContext = createContext<Theme>("light");

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setTheme] = useState<Theme>("light");
  return <ThemeContext.Provider value={theme}>{children}</ThemeContext.Provider>;
}

export const useTheme = () => useContext(ThemeContext);
```
- Split context into separate read and write providers to prevent unnecessary re-renders
- Prefer Zustand or Jotai for complex global state over deeply nested Context

## Suspense + Error Boundaries
```tsx
<ErrorBoundary fallback={<ErrorPage />}>
  <Suspense fallback={<Skeleton />}>
    <AsyncProductList />
  </Suspense>
</ErrorBoundary>
```
Use `react-error-boundary` package for composable Error Boundary configuration.

## Performance Patterns
- `React.memo(Component)` — skip re-render when props are shallowly equal
- `useTransition` — mark state updates as non-urgent to keep UI responsive
- `useDeferredValue` — defer expensive re-renders triggered by fast user input
- Virtualize long lists with `@tanstack/virtual` (avoid rendering thousands of DOM nodes)

## TypeScript Typing
```tsx
interface ButtonProps {
  variant: "primary" | "secondary";
  onClick: () => void;
  children: ReactNode;
  disabled?: boolean;
}
```
Extend `ComponentPropsWithoutRef<"button">` to forward native HTML attributes automatically.

## Testing with React Testing Library
```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

test("increments count on button click", async () => {
  render(<Counter />);
  await userEvent.click(screen.getByRole("button", { name: /increment/i }));
  expect(screen.getByText("1")).toBeInTheDocument();
});
```
