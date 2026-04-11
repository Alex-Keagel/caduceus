import { useEffect, useMemo, useRef, useState } from "react";
import { keybindingsGet } from "../api/tauri";
import type { Keybinding, KeybindingConfig } from "../types";
import {
  comboFromEvent,
  detectPlatform,
  isMatch,
  mergeKeybindings,
  shouldPreventDefault,
  type Platform,
} from "../keybindings";

const DEFAULT_CONFIG: KeybindingConfig = {
  preset: "intellij",
  overrides: [],
};

export function useKeybindings(actions: Record<string, () => void>) {
  const [config, setConfig] = useState<KeybindingConfig>(DEFAULT_CONFIG);
  const actionsRef = useRef(actions);
  const [platform, setPlatform] = useState<Platform>(() => detectPlatform());

  useEffect(() => {
    actionsRef.current = actions;
  }, [actions]);

  useEffect(() => {
    setPlatform(detectPlatform());
    keybindingsGet()
      .then((loaded) => setConfig(loaded))
      .catch((error) => {
        console.error("Failed to load keybindings", error);
      });
  }, []);

  const bindings = useMemo(() => mergeKeybindings(config), [config]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const activeEl = document.activeElement;
      const inEditable =
        activeEl instanceof HTMLInputElement ||
        activeEl instanceof HTMLTextAreaElement ||
        Boolean(activeEl?.getAttribute("contenteditable"));

      for (const binding of bindings) {
        if (inEditable && binding.context === "global" && binding.action !== "send_message") {
          continue;
        }
        if (!isMatch(event, binding, platform)) {
          continue;
        }

        const handler = actionsRef.current[binding.action];
        if (!handler) {
          continue;
        }

        if (shouldPreventDefault(binding.action)) {
          event.preventDefault();
        }

        handler();
        break;
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [bindings, platform]);

  return {
    config,
    platform,
    bindings,
    setConfig,
    comboFromEvent: (event: KeyboardEvent) => comboFromEvent(event, platform),
    reload: async () => {
      const loaded = await keybindingsGet();
      setConfig(loaded);
      return loaded;
    },
  };
}

export function findBinding(bindings: Keybinding[], action: string): Keybinding | undefined {
  return bindings.find((binding) => binding.action === action);
}
