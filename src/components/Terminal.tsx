import { useEffect, useRef, useCallback } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { listenPtyData, ptyWrite } from "../api/tauri";
import type { TerminalTab } from "../types";
import type { UnlistenFn } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";

interface TerminalProps {
  tabs: TerminalTab[];
  activeTabId: string;
  sessionId: string | null;
  onTabChange: (id: string) => void;
  onTabClose: (id: string) => void;
  onNewTab: () => void;
}

const CATPPUCCIN_THEME = {
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
};

export default function Terminal({
  tabs,
  activeTabId,
  sessionId,
  onTabChange,
  onTabClose,
  onNewTab,
}: TerminalProps) {
  const termInstances = useRef<Map<string, { term: XTerm; fit: FitAddon }>>(new Map());
  const initializedTabs = useRef<Set<string>>(new Set());
  const sessionIdRef = useRef<string | null>(sessionId);
  const activeTabIdRef = useRef<string>(activeTabId);

  useEffect(() => {
    sessionIdRef.current = sessionId;
  }, [sessionId]);

  useEffect(() => {
    activeTabIdRef.current = activeTabId;
  }, [activeTabId]);

  const initTab = useCallback((tabId: string, el: HTMLDivElement) => {
    if (initializedTabs.current.has(tabId)) return;
    initializedTabs.current.add(tabId);

    const term = new XTerm({
      theme: CATPPUCCIN_THEME,
      fontFamily: '"JetBrains Mono", "Fira Code", monospace',
      fontSize: 13,
      cursorBlink: true,
      scrollback: 10000,
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);

    try {
      const webglAddon = new WebglAddon();
      term.loadAddon(webglAddon);
    } catch {
      // WebGL not available
    }

    term.open(el);
    fitAddon.fit();

    term.onData((data) => {
      if (sessionIdRef.current) {
        ptyWrite(sessionIdRef.current, data).catch(console.error);
      }
    });

    termInstances.current.set(tabId, { term, fit: fitAddon });
  }, []);

  // Listen for PTY data
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    listenPtyData((payload) => {
      if (payload.session_id !== sessionIdRef.current) return;
      const inst = termInstances.current.get(activeTabIdRef.current);
      if (inst) inst.term.write(payload.data);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  // ResizeObserver for active tab
  useEffect(() => {
    const inst = termInstances.current.get(activeTabId);
    if (!inst) return;

    inst.fit.fit();

    const container = inst.term.element?.parentElement;
    if (!container) return;

    const ro = new ResizeObserver(() => {
      inst.fit.fit();
    });
    ro.observe(container);
    return () => ro.disconnect();
  }, [activeTabId]);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      termInstances.current.forEach((inst) => inst.term.dispose());
      termInstances.current.clear();
      initializedTabs.current.clear();
    };
  }, []);

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      <div className="terminal-tabs">
        {tabs.map((tab) => (
          <div
            key={tab.id}
            className={`terminal-tab ${tab.id === activeTabId ? "active" : ""}`}
            onClick={() => onTabChange(tab.id)}
          >
            {tab.title}
            {tabs.length > 1 && (
              <span
                className="terminal-tab-close"
                onClick={(e) => {
                  e.stopPropagation();
                  onTabClose(tab.id);
                }}
              >
                ✕
              </span>
            )}
          </div>
        ))}
        <div className="terminal-tab-new" onClick={onNewTab}>
          +
        </div>
      </div>

      {tabs.map((tab) => (
        <div
          key={tab.id}
          style={{
            flex: 1,
            overflow: "hidden",
            display: tab.id === activeTabId ? "block" : "none",
          }}
          ref={(el) => {
            if (el) initTab(tab.id, el);
          }}
        />
      ))}
    </div>
  );
}
