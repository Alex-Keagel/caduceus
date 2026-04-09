import { useEffect, useRef } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";

interface TerminalProps {
  sessionId: string | null;
}

export default function Terminal({ sessionId }: TerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const xtermRef = useRef<XTerm | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);

  useEffect(() => {
    if (!containerRef.current) return;

    const term = new XTerm({
      theme: {
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
      },
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
      // WebGL not available, fallback to canvas
    }

    term.open(containerRef.current);
    fitAddon.fit();

    term.writeln("\x1b[1;34m▶ Caduceus Terminal\x1b[0m");
    if (sessionId) {
      term.writeln(`\x1b[2mSession: ${sessionId}\x1b[0m`);
    }
    term.write("\r\n$ ");

    // Handle user input (basic echo for now)
    let inputBuffer = "";
    term.onKey(({ key, domEvent }) => {
      if (domEvent.key === "Enter") {
        term.write("\r\n$ ");
        inputBuffer = "";
      } else if (domEvent.key === "Backspace") {
        if (inputBuffer.length > 0) {
          inputBuffer = inputBuffer.slice(0, -1);
          term.write("\b \b");
        }
      } else {
        inputBuffer += key;
        term.write(key);
      }
    });

    xtermRef.current = term;
    fitAddonRef.current = fitAddon;

    const ro = new ResizeObserver(() => fitAddon.fit());
    ro.observe(containerRef.current);

    return () => {
      ro.disconnect();
      term.dispose();
    };
  }, []);

  useEffect(() => {
    if (xtermRef.current && sessionId) {
      xtermRef.current.writeln(`\r\n\x1b[2mSwitched to session: ${sessionId}\x1b[0m`);
    }
  }, [sessionId]);

  return (
    <div
      ref={containerRef}
      style={{ width: "100%", height: "100%", overflow: "hidden" }}
    />
  );
}
