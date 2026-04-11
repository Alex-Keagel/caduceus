import { useCallback, useEffect, useRef } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { listenPtyData, ptyClose, ptyCreate, ptyResize, ptyWrite } from "../api/tauri";
import type { TerminalTab } from "../types";
import type { UnlistenFn } from "@tauri-apps/api/event";
import type { ThemeDefinition } from "../theme";
import "@xterm/xterm/css/xterm.css";

interface TerminalProps {
  tabs: TerminalTab[];
  activeTabId: string;
  onTabChange: (id: string) => void;
  onTabClose: (id: string) => void;
  onNewTab: () => void;
  terminalTheme: ThemeDefinition["terminal"];
}

interface TerminalInstance {
  term: XTerm;
  fit: FitAddon;
  ptyId?: string;
  element?: HTMLDivElement;
}

function decodeBase64(value: string): string {
  const binary = window.atob(value);
  const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

export default function Terminal({
  tabs,
  activeTabId,
  onTabChange,
  onTabClose,
  onNewTab,
  terminalTheme,
}: TerminalProps) {
  const termInstances = useRef<Map<string, TerminalInstance>>(new Map());
  const initializedTabs = useRef<Set<string>>(new Set());
  const activeTabIdRef = useRef<string>(activeTabId);

  useEffect(() => {
    activeTabIdRef.current = activeTabId;
  }, [activeTabId]);

  const initTab = useCallback(
    async (tabId: string, element: HTMLDivElement) => {
      if (initializedTabs.current.has(tabId)) {
        const instance = termInstances.current.get(tabId);
        if (instance) {
          instance.element = element;
        }
        return;
      }
      initializedTabs.current.add(tabId);

      const term = new XTerm({
        theme: terminalTheme,
        fontFamily: '"JetBrains Mono", "Fira Code", monospace',
        fontSize: 13,
        cursorBlink: true,
        scrollback: 10000,
      });
      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);

      try {
        term.loadAddon(new WebglAddon());
      } catch {
        // Ignore WebGL failures.
      }

      term.open(element);
      fitAddon.fit();

      const response = await ptyCreate(term.cols, term.rows);
      const instance: TerminalInstance = { term, fit: fitAddon, ptyId: response.pty_id, element };

      term.onData((data) => {
        if (!instance.ptyId) return;
        void ptyWrite(instance.ptyId, data);
      });

      termInstances.current.set(tabId, instance);
      if (instance.ptyId) {
        void ptyResize(instance.ptyId, term.cols, term.rows);
      }
    },
    [terminalTheme]
  );

  useEffect(() => {
    termInstances.current.forEach((instance) => {
      instance.term.options.theme = terminalTheme;
      instance.term.refresh(0, instance.term.rows - 1);
    });
  }, [terminalTheme]);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    void listenPtyData((payload) => {
      const decoded = decodeBase64(payload.data);
      for (const instance of termInstances.current.values()) {
        if (instance.ptyId === payload.pty_id) {
          instance.term.write(decoded);
          break;
        }
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    const tabIds = new Set(tabs.map((tab) => tab.id));
    const removed = Array.from(termInstances.current.entries()).filter(([tabId]) => !tabIds.has(tabId));
    removed.forEach(([tabId, instance]) => {
      if (instance.ptyId) {
        void ptyClose(instance.ptyId);
      }
      instance.term.dispose();
      termInstances.current.delete(tabId);
      initializedTabs.current.delete(tabId);
    });
  }, [tabs]);

  useEffect(() => {
    const instance = termInstances.current.get(activeTabId);
    if (!instance) return;

    instance.fit.fit();
    if (instance.ptyId) {
      void ptyResize(instance.ptyId, instance.term.cols, instance.term.rows);
    }

    const container = instance.element;
    if (!container) return;

    const observer = new ResizeObserver(() => {
      instance.fit.fit();
      if (instance.ptyId) {
        void ptyResize(instance.ptyId, instance.term.cols, instance.term.rows);
      }
    });
    observer.observe(container);
    return () => observer.disconnect();
  }, [activeTabId]);

  useEffect(() => {
    return () => {
      termInstances.current.forEach((instance) => {
        if (instance.ptyId) {
          void ptyClose(instance.ptyId);
        }
        instance.term.dispose();
      });
      termInstances.current.clear();
      initializedTabs.current.clear();
    };
  }, []);

  return (
    <div className="terminal-shell">
      <div className="terminal-tabs">
        {tabs.map((tab) => (
          <div
            key={tab.id}
            className={`terminal-tab ${tab.id === activeTabId ? "active" : ""}`}
            onClick={() => onTabChange(tab.id)}
          >
            {tab.title}
            {tabs.length > 1 ? (
              <span
                className="terminal-tab-close"
                onClick={(event) => {
                  event.stopPropagation();
                  onTabClose(tab.id);
                }}
              >
                ✕
              </span>
            ) : null}
          </div>
        ))}
        <div className="terminal-tab-new" onClick={onNewTab}>
          +
        </div>
      </div>

      {tabs.map((tab) => (
        <div
          key={tab.id}
          className="terminal-stage"
          style={{ display: tab.id === activeTabId ? "block" : "none" }}
          ref={(element) => {
            if (element) {
              void initTab(tab.id, element);
            }
          }}
        />
      ))}
    </div>
  );
}
