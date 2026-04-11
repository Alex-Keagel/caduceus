import { useEffect, useMemo, useState } from "react";
import { keybindingsGet, keybindingsPresets, keybindingsSet } from "../api/tauri";
import { ACTION_DEFINITIONS, comboFromEvent, detectPlatform, mergeKeybindings } from "../keybindings";
import type { KeybindingConfig, KeybindingPreset } from "../types";

interface KeybindingSettingsProps {
  onClose: () => void;
  onSaved?: (config: KeybindingConfig) => void;
}

const DEFAULT_CONFIG: KeybindingConfig = {
  preset: "intellij",
  overrides: [],
};

const PRESET_LABELS: Record<KeybindingPreset, string> = {
  intellij: "IntelliJ",
  vscode: "VS Code",
  vim: "Vim",
  emacs: "Emacs",
  custom: "Custom",
};

export default function KeybindingSettings({ onClose, onSaved }: KeybindingSettingsProps) {
  const [config, setConfig] = useState<KeybindingConfig>(DEFAULT_CONFIG);
  const [presets, setPresets] = useState<KeybindingPreset[]>(["intellij", "vscode", "vim", "emacs", "custom"]);
  const [recordingAction, setRecordingAction] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    keybindingsGet().then(setConfig).catch(console.error);
    keybindingsPresets().then(setPresets).catch(console.error);
  }, []);

  const platform = detectPlatform();
  const effectiveBindings = useMemo(() => mergeKeybindings(config), [config]);
  const keysByAction = useMemo(() => {
    const map = new Map<string, string>();
    for (const binding of effectiveBindings) {
      map.set(binding.action, binding.keys);
    }
    return map;
  }, [effectiveBindings]);

  useEffect(() => {
    if (!recordingAction) return;

    const onKeyDown = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();

      if (["Meta", "Control", "Shift", "Alt"].includes(event.key)) {
        return;
      }

      const combo = comboFromEvent(event, platform);
      const definition = ACTION_DEFINITIONS.find((item) => item.action === recordingAction);
      const context = definition?.context ?? "global";

      setConfig((current) => {
        const nextOverrides = [...current.overrides];
        const idx = nextOverrides.findIndex(
          (binding) => binding.action === recordingAction && (binding.context ?? "global") === context
        );

        const override = {
          action: recordingAction,
          keys: combo,
          context,
        };

        if (idx >= 0) {
          nextOverrides[idx] = override;
        } else {
          nextOverrides.push(override);
        }

        return { ...current, overrides: nextOverrides };
      });

      setRecordingAction(null);
      setStatus(`Recorded ${combo} for ${recordingAction}`);
    };

    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [platform, recordingAction]);

  const save = async () => {
    await keybindingsSet(config);
    setStatus("Saved keybindings");
    onSaved?.(config);
  };

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "#00000088",
        zIndex: 1200,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
      onClick={onClose}
    >
      <div
        onClick={(event) => event.stopPropagation()}
        style={{
          width: "min(900px, 92vw)",
          maxHeight: "88vh",
          overflow: "auto",
          background: "#1e1e2e",
          border: "1px solid #45475a",
          borderRadius: 10,
          padding: 16,
        }}
      >
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 12 }}>
          <h3 style={{ fontSize: 14 }}>Keybinding Settings</h3>
          <button onClick={onClose} style={buttonStyle("#313244", "#cdd6f4")}>Close</button>
        </div>

        <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12 }}>
          <label htmlFor="preset">Preset:</label>
          <select
            id="preset"
            value={config.preset}
            onChange={(event) =>
              setConfig({
                preset: event.target.value as KeybindingPreset,
                overrides: [],
              })
            }
            style={{ background: "#313244", color: "#cdd6f4", border: "1px solid #45475a", borderRadius: 6, padding: "6px 8px" }}
          >
            {presets.map((preset) => (
              <option key={preset} value={preset}>
                {PRESET_LABELS[preset] ?? preset}
              </option>
            ))}
          </select>

          <button
            type="button"
            onClick={() => setConfig({ preset: config.preset, overrides: [] })}
            style={buttonStyle("#45475a", "#cdd6f4")}
          >
            Reset to preset defaults
          </button>

          <button type="button" onClick={save} style={buttonStyle("#89b4fa", "#1e1e2e")}>
            Save
          </button>
        </div>

        {status && <div style={{ color: "#a6e3a1", marginBottom: 10, fontSize: 12 }}>{status}</div>}

        <table style={{ width: "100%", borderCollapse: "collapse" }}>
          <thead>
            <tr style={{ background: "#181825", textAlign: "left" }}>
              <th style={thStyle}>Action</th>
              <th style={thStyle}>Context</th>
              <th style={thStyle}>Keybinding</th>
            </tr>
          </thead>
          <tbody>
            {ACTION_DEFINITIONS.map((definition) => (
              <tr key={definition.action} style={{ borderBottom: "1px solid #313244" }}>
                <td style={tdStyle}>{definition.label}</td>
                <td style={tdStyle}>{definition.context}</td>
                <td style={tdStyle}>
                  <button
                    type="button"
                    onClick={() => setRecordingAction(definition.action)}
                    style={{
                      ...buttonStyle(recordingAction === definition.action ? "#f9e2af" : "#313244", "#cdd6f4"),
                      width: "100%",
                      textAlign: "left",
                    }}
                  >
                    {recordingAction === definition.action
                      ? "Press keys…"
                      : keysByAction.get(definition.action) ?? "Unbound"}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function buttonStyle(background: string, color: string) {
  return {
    border: "none",
    borderRadius: 6,
    background,
    color,
    padding: "6px 10px",
    fontWeight: 600,
    cursor: "pointer",
  } as const;
}

const thStyle = {
  padding: "8px 10px",
  fontSize: 11,
  color: "#6c7086",
} as const;

const tdStyle = {
  padding: "8px 10px",
  fontSize: 12,
} as const;
