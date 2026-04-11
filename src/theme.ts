import type { CSSProperties } from "react";

export const THEME_STORAGE_KEY = "caduceus:theme";

export type ThemeName =
  | "catppuccin-mocha"
  | "catppuccin-latte"
  | "one-dark"
  | "dracula"
  | "nord";

export interface ThemeDefinition {
  id: ThemeName;
  label: string;
  mode: "dark" | "light";
  variables: Record<string, string>;
  terminal: {
    background: string;
    foreground: string;
    cursor: string;
    selectionBackground: string;
    black: string;
    red: string;
    green: string;
    yellow: string;
    blue: string;
    magenta: string;
    cyan: string;
    white: string;
  };
}

const sharedTerminal = (overrides: Partial<ThemeDefinition["terminal"]>): ThemeDefinition["terminal"] => ({
  background: "#1e1e2e",
  foreground: "#cdd6f4",
  cursor: "#f5e0dc",
  selectionBackground: "#45475a",
  black: "#45475a",
  red: "#f38ba8",
  green: "#a6e3a1",
  yellow: "#f9e2af",
  blue: "#89b4fa",
  magenta: "#cba6f7",
  cyan: "#89dceb",
  white: "#bac2de",
  ...overrides,
});

export const THEMES: ThemeDefinition[] = [
  {
    id: "catppuccin-mocha",
    label: "Catppuccin Mocha",
    mode: "dark",
    variables: {
      "--color-bg": "#1e1e2e",
      "--color-surface": "#181825",
      "--color-panel": "#313244",
      "--color-panel-alt": "#11111b",
      "--color-border": "#45475a",
      "--color-text": "#cdd6f4",
      "--color-muted": "#6c7086",
      "--color-accent": "#89b4fa",
      "--color-accent-strong": "#cba6f7",
      "--color-success": "#a6e3a1",
      "--color-warning": "#f9e2af",
      "--color-danger": "#f38ba8",
      "--color-chat-user": "rgba(137, 180, 250, 0.15)",
      "--color-code-bg": "#11111b",
      "--color-overlay": "rgba(17, 17, 27, 0.94)",
      "--color-shadow": "rgba(0, 0, 0, 0.35)",
    },
    terminal: sharedTerminal({}),
  },
  {
    id: "catppuccin-latte",
    label: "Catppuccin Latte",
    mode: "light",
    variables: {
      "--color-bg": "#eff1f5",
      "--color-surface": "#e6e9ef",
      "--color-panel": "#dce0e8",
      "--color-panel-alt": "#ccd0da",
      "--color-border": "#bcc0cc",
      "--color-text": "#4c4f69",
      "--color-muted": "#7c7f93",
      "--color-accent": "#1e66f5",
      "--color-accent-strong": "#8839ef",
      "--color-success": "#40a02b",
      "--color-warning": "#df8e1d",
      "--color-danger": "#d20f39",
      "--color-chat-user": "rgba(30, 102, 245, 0.12)",
      "--color-code-bg": "#ccd0da",
      "--color-overlay": "rgba(239, 241, 245, 0.95)",
      "--color-shadow": "rgba(76, 79, 105, 0.16)",
    },
    terminal: sharedTerminal({
      background: "#eff1f5",
      foreground: "#4c4f69",
      cursor: "#dc8a78",
      selectionBackground: "#bcc0cc",
      black: "#5c5f77",
      red: "#d20f39",
      green: "#40a02b",
      yellow: "#df8e1d",
      blue: "#1e66f5",
      magenta: "#8839ef",
      cyan: "#179299",
      white: "#4c4f69",
    }),
  },
  {
    id: "one-dark",
    label: "One Dark",
    mode: "dark",
    variables: {
      "--color-bg": "#282c34",
      "--color-surface": "#21252b",
      "--color-panel": "#2c313c",
      "--color-panel-alt": "#1f2329",
      "--color-border": "#3e4451",
      "--color-text": "#abb2bf",
      "--color-muted": "#5c6370",
      "--color-accent": "#61afef",
      "--color-accent-strong": "#c678dd",
      "--color-success": "#98c379",
      "--color-warning": "#e5c07b",
      "--color-danger": "#e06c75",
      "--color-chat-user": "rgba(97, 175, 239, 0.16)",
      "--color-code-bg": "#1f2329",
      "--color-overlay": "rgba(33, 37, 43, 0.94)",
      "--color-shadow": "rgba(0, 0, 0, 0.4)",
    },
    terminal: sharedTerminal({
      background: "#282c34",
      foreground: "#abb2bf",
      cursor: "#528bff",
      selectionBackground: "#3e4451",
      black: "#5c6370",
      red: "#e06c75",
      green: "#98c379",
      yellow: "#e5c07b",
      blue: "#61afef",
      magenta: "#c678dd",
      cyan: "#56b6c2",
      white: "#dcdfe4",
    }),
  },
  {
    id: "dracula",
    label: "Dracula",
    mode: "dark",
    variables: {
      "--color-bg": "#282a36",
      "--color-surface": "#21222c",
      "--color-panel": "#343746",
      "--color-panel-alt": "#1d1f28",
      "--color-border": "#44475a",
      "--color-text": "#f8f8f2",
      "--color-muted": "#6272a4",
      "--color-accent": "#8be9fd",
      "--color-accent-strong": "#bd93f9",
      "--color-success": "#50fa7b",
      "--color-warning": "#f1fa8c",
      "--color-danger": "#ff5555",
      "--color-chat-user": "rgba(139, 233, 253, 0.14)",
      "--color-code-bg": "#1d1f28",
      "--color-overlay": "rgba(33, 34, 44, 0.94)",
      "--color-shadow": "rgba(0, 0, 0, 0.45)",
    },
    terminal: sharedTerminal({
      background: "#282a36",
      foreground: "#f8f8f2",
      cursor: "#f8f8f2",
      selectionBackground: "#44475a",
      black: "#44475a",
      red: "#ff5555",
      green: "#50fa7b",
      yellow: "#f1fa8c",
      blue: "#8be9fd",
      magenta: "#bd93f9",
      cyan: "#8be9fd",
      white: "#f8f8f2",
    }),
  },
  {
    id: "nord",
    label: "Nord",
    mode: "dark",
    variables: {
      "--color-bg": "#2e3440",
      "--color-surface": "#242933",
      "--color-panel": "#3b4252",
      "--color-panel-alt": "#232831",
      "--color-border": "#434c5e",
      "--color-text": "#eceff4",
      "--color-muted": "#81a1c1",
      "--color-accent": "#88c0d0",
      "--color-accent-strong": "#b48ead",
      "--color-success": "#a3be8c",
      "--color-warning": "#ebcb8b",
      "--color-danger": "#bf616a",
      "--color-chat-user": "rgba(136, 192, 208, 0.14)",
      "--color-code-bg": "#232831",
      "--color-overlay": "rgba(36, 41, 51, 0.94)",
      "--color-shadow": "rgba(15, 17, 21, 0.45)",
    },
    terminal: sharedTerminal({
      background: "#2e3440",
      foreground: "#eceff4",
      cursor: "#d8dee9",
      selectionBackground: "#434c5e",
      black: "#3b4252",
      red: "#bf616a",
      green: "#a3be8c",
      yellow: "#ebcb8b",
      blue: "#81a1c1",
      magenta: "#b48ead",
      cyan: "#88c0d0",
      white: "#e5e9f0",
    }),
  },
];

export const DEFAULT_THEME: ThemeName = "catppuccin-mocha";

export function getTheme(themeName: ThemeName): ThemeDefinition {
  return THEMES.find((theme) => theme.id === themeName) ?? THEMES[0];
}

export function loadStoredTheme(): ThemeName {
  if (typeof window === "undefined") {
    return DEFAULT_THEME;
  }
  const stored = window.localStorage.getItem(THEME_STORAGE_KEY) as ThemeName | null;
  return THEMES.some((theme) => theme.id === stored) ? stored! : DEFAULT_THEME;
}

export function applyTheme(themeName: ThemeName): ThemeDefinition {
  const theme = getTheme(themeName);
  if (typeof document !== "undefined") {
    const root = document.documentElement;
    root.dataset.theme = theme.id;
    root.style.colorScheme = theme.mode;
    for (const [key, value] of Object.entries(theme.variables)) {
      root.style.setProperty(key, value);
    }
  }
  if (typeof window !== "undefined") {
    window.localStorage.setItem(THEME_STORAGE_KEY, theme.id);
  }
  return theme;
}

export const panelStyle: CSSProperties = {
  background: "var(--color-surface)",
  border: "1px solid var(--color-border)",
  color: "var(--color-text)",
};
