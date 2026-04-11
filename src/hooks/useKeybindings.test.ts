import assert from "node:assert/strict";
import test from "node:test";
import {
  comboFromEvent,
  detectPlatform,
  getPresetBindings,
  isMatch,
  mergeKeybindings,
  normalizeCombo,
  resolvePlatformShortcut,
} from "../keybindings";

test("preset loading returns command palette binding", () => {
  const bindings = getPresetBindings("intellij");
  const item = bindings.find((binding) => binding.action === "command_palette");
  assert.equal(item?.keys, "Ctrl+Shift+A / Cmd+Shift+A");
});

test("platform detection resolves mac platform", () => {
  assert.equal(detectPlatform("MacIntel"), "mac");
  assert.equal(detectPlatform("Win32"), "other");
});

test("override merging replaces preset binding", () => {
  const merged = mergeKeybindings({
    preset: "vscode",
    overrides: [{ action: "command_palette", keys: "Ctrl+K", context: "global" }],
  });
  assert.equal(merged.find((binding) => binding.action === "command_palette")?.keys, "Ctrl+K");
});

test("platform shortcut resolves cmd variant on mac", () => {
  assert.equal(resolvePlatformShortcut("Ctrl+Shift+P / Cmd+Shift+P", "mac"), "Cmd+Shift+P");
  assert.equal(resolvePlatformShortcut("Ctrl+Shift+P / Cmd+Shift+P", "other"), "Ctrl+Shift+P");
});

test("combo normalization orders modifiers", () => {
  assert.equal(normalizeCombo("Shift+Ctrl+p"), "Ctrl+Shift+P");
});

test("key matching maps keyboard event to action", () => {
  const fakeEvent = {
    key: "p",
    ctrlKey: true,
    metaKey: false,
    altKey: false,
    shiftKey: true,
    repeat: false,
    isComposing: false,
  } as KeyboardEvent;

  const combo = comboFromEvent(fakeEvent, "other");
  assert.equal(combo, "Ctrl+Shift+P");

  assert.equal(
    isMatch(fakeEvent, { action: "command_palette", keys: "Ctrl+Shift+P", context: "global" }, "other"),
    true
  );
});
