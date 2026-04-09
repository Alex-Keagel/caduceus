import { useState, useCallback } from "react";
import Terminal from "./components/Terminal";
import Chat from "./components/Chat";
import GitPanel from "./components/GitPanel";
import StatusBar from "./components/StatusBar";
import CommandPalette from "./components/CommandPalette";
import type { SessionInfo } from "./types";

function App() {
  const [activeSession, setActiveSession] = useState<SessionInfo | null>(null);
  const [commandPaletteOpen, setCommandPaletteOpen] = useState(false);

  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "p") {
      e.preventDefault();
      setCommandPaletteOpen((o) => !o);
    }
  }, []);

  return (
    <div className="app-layout" onKeyDown={handleKeyDown} tabIndex={-1}>
      {/* Sidebar */}
      <aside className="sidebar">
        <GitPanel projectRoot={activeSession?.project_root ?? null} />
      </aside>

      {/* Main pane: terminal */}
      <main className="main-area">
        <div className="terminal-pane">
          <Terminal sessionId={activeSession?.id ?? null} />
        </div>
      </main>

      {/* Right pane: AI chat */}
      <aside className="chat-pane">
        <Chat
          session={activeSession}
          onSessionCreated={setActiveSession}
        />
      </aside>

      {/* Status bar */}
      <StatusBar session={activeSession} />

      {/* Command palette overlay */}
      {commandPaletteOpen && (
        <CommandPalette
          onClose={() => setCommandPaletteOpen(false)}
          onSessionSelect={setActiveSession}
        />
      )}
    </div>
  );
}

export default App;
