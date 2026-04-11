import type { Keybinding, KeybindingConfig, KeybindingPreset } from "./types";

export type Platform = "mac" | "other";

export interface ActionDefinition {
  action: string;
  label: string;
  context: string;
  intellij: string;
  vscode: string;
  vim: string;
  emacs: string;
}

export const ACTION_DEFINITIONS: ActionDefinition[] = [
  {
    action: "command_palette",
    label: "Command Palette",
    context: "global",
    intellij: "Ctrl+Shift+A / Cmd+Shift+A",
    vscode: "Ctrl+Shift+P / Cmd+Shift+P",
    vim: ":",
    emacs: "Alt+X",
  },
  {
    action: "new_terminal_tab",
    label: "New Terminal Tab",
    context: "terminal",
    intellij: "Alt+F12",
    vscode: "Ctrl+`",
    vim: ":term",
    emacs: "Ctrl+Alt+T",
  },
  { action: "close_tab", label: "Close Tab", context: "global", intellij: "Ctrl+F4", vscode: "Ctrl+W", vim: ":q", emacs: "Ctrl+X K" },
  { action: "split_horizontal", label: "Split Horizontal", context: "terminal", intellij: "Ctrl+Shift+H", vscode: "Ctrl+\\", vim: ":sp", emacs: "Ctrl+X 2" },
  { action: "split_vertical", label: "Split Vertical", context: "terminal", intellij: "Ctrl+Shift+V", vscode: "Ctrl+Shift+\\", vim: ":vsp", emacs: "Ctrl+X 3" },
  { action: "toggle_chat", label: "Toggle Chat", context: "global", intellij: "Alt+C", vscode: "Ctrl+Shift+I", vim: "<leader>c", emacs: "Ctrl+C C" },
  { action: "toggle_git_panel", label: "Toggle Git Panel", context: "global", intellij: "Alt+9", vscode: "Ctrl+Shift+G", vim: "<leader>g", emacs: "Ctrl+C G" },
  { action: "toggle_marketplace", label: "Toggle Marketplace", context: "global", intellij: "Alt+M", vscode: "Ctrl+Shift+M", vim: "<leader>m", emacs: "Ctrl+C M" },
  { action: "toggle_kanban", label: "Toggle Kanban", context: "global", intellij: "Alt+K", vscode: "Ctrl+Shift+K", vim: "<leader>k", emacs: "Ctrl+C K" },
  { action: "send_message", label: "Send Message", context: "chat", intellij: "Ctrl+Enter", vscode: "Ctrl+Enter", vim: "<CR>", emacs: "Ctrl+Enter" },
  { action: "cancel_agent", label: "Cancel Agent", context: "chat", intellij: "Ctrl+C", vscode: "Ctrl+C", vim: "<Esc>", emacs: "Ctrl+G" },
  { action: "focus_terminal", label: "Focus Terminal", context: "global", intellij: "Alt+F12", vscode: "Ctrl+`", vim: "<leader>t", emacs: "Ctrl+Alt+T" },
  { action: "focus_chat", label: "Focus Chat", context: "global", intellij: "Alt+C", vscode: "Ctrl+L", vim: "<leader>c", emacs: "Ctrl+X O" },
  { action: "next_tab", label: "Next Tab", context: "global", intellij: "Alt+Right", vscode: "Ctrl+Tab", vim: "gt", emacs: "Ctrl+PageDown" },
  { action: "prev_tab", label: "Prev Tab", context: "global", intellij: "Alt+Left", vscode: "Ctrl+Shift+Tab", vim: "gT", emacs: "Ctrl+PageUp" },
  { action: "search_files", label: "Search Files", context: "global", intellij: "Ctrl+Shift+F", vscode: "Ctrl+Shift+F", vim: "/", emacs: "Ctrl+S" },
  { action: "quick_open", label: "Quick Open", context: "global", intellij: "Ctrl+Shift+N", vscode: "Ctrl+P", vim: ":e", emacs: "Ctrl+X Ctrl+F" },
  { action: "settings", label: "Settings", context: "global", intellij: "Ctrl+Alt+S", vscode: "Ctrl+,", vim: ":set", emacs: "Ctrl+H V" },
  { action: "switch_mode", label: "Switch Mode", context: "global", intellij: "Ctrl+Shift+M", vscode: "Ctrl+Shift+M", vim: ":mode", emacs: "Ctrl+C Ctrl+M" },
  { action: "checkpoint", label: "Checkpoint", context: "global", intellij: "Ctrl+S", vscode: "Ctrl+S", vim: ":w", emacs: "Ctrl+X Ctrl+S" },
];

export function detectPlatform(value?: string): Platform {
  const platform = value ?? (typeof navigator === "undefined" ? "" : navigator.platform);
  return /mac|iphone|ipad|ipod/i.test(platform) ? "mac" : "other";
}

export function resolvePlatformShortcut(keys: string, platform: Platform): string {
  const normalized = keys.trim();
  if (!normalized.includes("/")) return normalized;
  const candidates = normalized.split("/").map((item) => item.trim());
  const prefer = platform === "mac" ? "cmd+" : "ctrl+";
  return candidates.find((item) => item.toLowerCase().includes(prefer)) ?? candidates[0] ?? normalized;
}

export function getPresetBindings(preset: KeybindingPreset): Keybinding[] {
  if (preset === "custom") {
    return [];
  }

  const key = preset as Exclude<KeybindingPreset, "custom">;
  return ACTION_DEFINITIONS.map((definition) => ({
    action: definition.action,
    keys: definition[key],
    context: definition.context,
  }));
}

export function mergeKeybindings(config: KeybindingConfig): Keybinding[] {
  const merged = [...getPresetBindings(config.preset)];
  for (const override of config.overrides) {
    const existingIndex = merged.findIndex(
      (binding) => binding.action === override.action && (binding.context ?? "global") === (override.context ?? "global")
    );
    if (existingIndex >= 0) {
      merged[existingIndex] = override;
    } else {
      merged.push(override);
    }
  }
  return merged;
}

export function normalizeCombo(combo: string): string {
  const parts = combo
    .split("+")
    .map((part) => part.trim())
    .filter(Boolean)
    .map(normalizeToken);

  const modifiers = ["Cmd", "Ctrl", "Alt", "Shift"];
  const ordered = [
    ...modifiers.filter((modifier) => parts.includes(modifier)),
    ...parts.filter((part) => !modifiers.includes(part)),
  ];
  return ordered.join("+");
}

function normalizeToken(token: string): string {
  const value = token.toLowerCase();
  if (["meta", "cmd", "command", "⌘"].includes(value)) return "Cmd";
  if (["control", "ctrl", "^"].includes(value)) return "Ctrl";
  if (["option", "alt", "⌥"].includes(value)) return "Alt";
  if (["shift", "⇧"].includes(value)) return "Shift";
  if (["escape", "esc"].includes(value)) return "Escape";
  if (value === "return") return "Enter";
  if (value === "space") return "Space";
  if (value === "arrowleft") return "Left";
  if (value === "arrowright") return "Right";
  if (value === "arrowup") return "Up";
  if (value === "arrowdown") return "Down";
  if (token.length === 1) return token.toUpperCase();
  return token;
}

function keyFromEvent(event: KeyboardEvent): string {
  if (event.key === " ") return "Space";
  if (event.key === "ArrowLeft") return "Left";
  if (event.key === "ArrowRight") return "Right";
  if (event.key === "ArrowUp") return "Up";
  if (event.key === "ArrowDown") return "Down";
  if (event.key.length === 1) return event.key.toUpperCase();
  return normalizeToken(event.key);
}

export function comboFromEvent(event: KeyboardEvent, platform: Platform): string {
  const combo: string[] = [];
  if (platform === "mac" ? event.metaKey : event.ctrlKey) combo.push(platform === "mac" ? "Cmd" : "Ctrl");
  if (platform === "mac" ? event.ctrlKey : event.metaKey) combo.push(platform === "mac" ? "Ctrl" : "Cmd");
  if (event.altKey) combo.push("Alt");
  if (event.shiftKey) combo.push("Shift");
  combo.push(keyFromEvent(event));
  return normalizeCombo(combo.join("+"));
}

function normalizeVimLiteral(shortcut: string): string {
  if (shortcut === "<CR>") return "Enter";
  if (shortcut === "<Esc>") return "Escape";
  return shortcut;
}

export function isMatch(event: KeyboardEvent, binding: Keybinding, platform: Platform): boolean {
  if (event.repeat || event.isComposing) return false;

  const resolved = resolvePlatformShortcut(binding.keys, platform);
  if (resolved.includes(" ") || resolved.includes("<leader>") || (/^:[a-z]/i.test(resolved) && resolved !== ":")) {
    return false;
  }

  const shortcut = normalizeVimLiteral(resolved);
  const normalizedShortcut = normalizeCombo(shortcut);
  const eventCombo = comboFromEvent(event, platform);
  return normalizedShortcut === eventCombo;
}

export function shouldPreventDefault(action: string): boolean {
  return [
    "command_palette",
    "close_tab",
    "search_files",
    "quick_open",
    "settings",
    "checkpoint",
    "new_terminal_tab",
  ].includes(action);
}
