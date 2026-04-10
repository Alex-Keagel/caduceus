import { useReducer, useCallback } from "react";
import Terminal from "./components/Terminal";
import Chat from "./components/Chat";
import GitPanel from "./components/GitPanel";
import StatusBar from "./components/StatusBar";
import CommandPalette from "./components/CommandPalette";
import type { SessionInfo, SessionPhase, TokenUsage, ProjectScanResult, TerminalTab } from "./types";

interface AppState {
  sessions: SessionInfo[];
  activeSessionId: string | null;
  tabs: TerminalTab[];
  activeTabId: string;
  commandPaletteOpen: boolean;
  phase: SessionPhase;
  tokenUsage: TokenUsage | null;
  projectContext: ProjectScanResult | null;
}

type AppAction =
  | { type: "ADD_SESSION"; session: SessionInfo }
  | { type: "SET_ACTIVE_SESSION"; sessionId: string | null }
  | { type: "NEW_TAB" }
  | { type: "CLOSE_TAB"; tabId: string }
  | { type: "SET_ACTIVE_TAB"; tabId: string }
  | { type: "TOGGLE_COMMAND_PALETTE" }
  | { type: "CLOSE_COMMAND_PALETTE" }
  | { type: "SET_PHASE"; phase: SessionPhase }
  | { type: "SET_TOKEN_USAGE"; usage: TokenUsage }
  | { type: "SET_PROJECT_CONTEXT"; context: ProjectScanResult };

const initialTab: TerminalTab = { id: "tab-1", title: "Terminal 1" };

const initialState: AppState = {
  sessions: [],
  activeSessionId: null,
  tabs: [initialTab],
  activeTabId: initialTab.id,
  commandPaletteOpen: false,
  phase: "Idle",
  tokenUsage: null,
  projectContext: null,
};

let tabCounter = 1;

function reducer(state: AppState, action: AppAction): AppState {
  switch (action.type) {
    case "ADD_SESSION":
      return {
        ...state,
        sessions: [...state.sessions, action.session],
        activeSessionId: action.session.id,
        phase: action.session.phase,
      };
    case "SET_ACTIVE_SESSION":
      return { ...state, activeSessionId: action.sessionId };
    case "NEW_TAB": {
      tabCounter += 1;
      const newTab: TerminalTab = { id: `tab-${tabCounter}`, title: `Terminal ${tabCounter}` };
      return { ...state, tabs: [...state.tabs, newTab], activeTabId: newTab.id };
    }
    case "CLOSE_TAB": {
      const remaining = state.tabs.filter((t) => t.id !== action.tabId);
      if (remaining.length === 0) return state;
      const newActive = state.activeTabId === action.tabId
        ? remaining[remaining.length - 1].id
        : state.activeTabId;
      return { ...state, tabs: remaining, activeTabId: newActive };
    }
    case "SET_ACTIVE_TAB":
      return { ...state, activeTabId: action.tabId };
    case "TOGGLE_COMMAND_PALETTE":
      return { ...state, commandPaletteOpen: !state.commandPaletteOpen };
    case "CLOSE_COMMAND_PALETTE":
      return { ...state, commandPaletteOpen: false };
    case "SET_PHASE":
      return { ...state, phase: action.phase };
    case "SET_TOKEN_USAGE":
      return { ...state, tokenUsage: action.usage };
    case "SET_PROJECT_CONTEXT":
      return { ...state, projectContext: action.context };
  }
}

function App() {
  const [state, dispatch] = useReducer(reducer, initialState);

  const activeSession = state.sessions.find((s) => s.id === state.activeSessionId) ?? null;

  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.metaKey && e.key === "k") {
      e.preventDefault();
      dispatch({ type: "TOGGLE_COMMAND_PALETTE" });
    } else if (e.metaKey && e.key === "t") {
      e.preventDefault();
      dispatch({ type: "NEW_TAB" });
    }
  }, []);

  return (
    <div className="app-layout" onKeyDown={handleKeyDown} tabIndex={-1}>
      <aside className="sidebar">
        <GitPanel projectRoot={activeSession?.project_root ?? null} />
      </aside>

      <main className="main-area">
        <Terminal
          tabs={state.tabs}
          activeTabId={state.activeTabId}
          sessionId={state.activeSessionId}
          onTabChange={(id) => dispatch({ type: "SET_ACTIVE_TAB", tabId: id })}
          onTabClose={(id) => dispatch({ type: "CLOSE_TAB", tabId: id })}
          onNewTab={() => dispatch({ type: "NEW_TAB" })}
        />
      </main>

      <aside className="chat-pane">
        <Chat
          session={activeSession}
          onSessionCreated={(s) => dispatch({ type: "ADD_SESSION", session: s })}
          onPhaseChange={(phase) => dispatch({ type: "SET_PHASE", phase })}
          onTokenUsage={(usage) => dispatch({ type: "SET_TOKEN_USAGE", usage })}
        />
      </aside>

      <StatusBar
        session={activeSession}
        phase={state.phase}
        tokenUsage={state.tokenUsage}
        projectContext={state.projectContext}
      />

      {state.commandPaletteOpen && (
        <CommandPalette
          onClose={() => dispatch({ type: "CLOSE_COMMAND_PALETTE" })}
          onSessionSelect={(s) => {
            dispatch({ type: "SET_ACTIVE_SESSION", sessionId: s.id });
            dispatch({ type: "CLOSE_COMMAND_PALETTE" });
          }}
        />
      )}
    </div>
  );
}

export default App;
