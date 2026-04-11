import { useMemo, useReducer, useRef } from "react";
import Terminal from "./components/Terminal";
import Chat, { type ChatHandle } from "./components/Chat";
import GitPanel from "./components/GitPanel";
import StatusBar from "./components/StatusBar";
import CommandPalette from "./components/CommandPalette";
import MarketplacePanel from "./components/MarketplacePanel";
import KanbanBoard from "./components/KanbanBoard";
import KeybindingSettings from "./components/KeybindingSettings";
import { useKeybindings } from "./hooks/useKeybindings";
import type {
  SessionInfo,
  SessionPhase,
  TokenUsage,
  ProjectScanResult,
  TerminalTab,
  KanbanBoard as KanbanBoardData,
} from "./types";

interface AppState {
  sessions: SessionInfo[];
  activeSessionId: string | null;
  tabs: TerminalTab[];
  activeTabId: string;
  commandPaletteOpen: boolean;
  marketplaceOpen: boolean;
  kanbanOpen: boolean;
  gitPanelOpen: boolean;
  chatOpen: boolean;
  keybindingSettingsOpen: boolean;
  phase: SessionPhase;
  tokenUsage: TokenUsage | null;
  projectContext: ProjectScanResult | null;
  kanbanBoard: KanbanBoardData | null;
}

type AppAction =
  | { type: "ADD_SESSION"; session: SessionInfo }
  | { type: "SET_ACTIVE_SESSION"; sessionId: string | null }
  | { type: "NEW_TAB" }
  | { type: "CLOSE_TAB"; tabId: string }
  | { type: "SET_ACTIVE_TAB"; tabId: string }
  | { type: "TOGGLE_COMMAND_PALETTE" }
  | { type: "CLOSE_COMMAND_PALETTE" }
  | { type: "TOGGLE_MARKETPLACE" }
  | { type: "TOGGLE_GIT_PANEL" }
  | { type: "TOGGLE_CHAT" }
  | { type: "TOGGLE_KEYBINDING_SETTINGS" }
  | { type: "OPEN_KEYBINDING_SETTINGS" }
  | { type: "CLOSE_KEYBINDING_SETTINGS" }
  | { type: "OPEN_KANBAN"; board: KanbanBoardData }
  | { type: "CLOSE_KANBAN" }
  | { type: "TOGGLE_KANBAN" }
  | { type: "SET_KANBAN_BOARD"; board: KanbanBoardData }
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
  marketplaceOpen: false,
  kanbanOpen: false,
  gitPanelOpen: true,
  chatOpen: true,
  keybindingSettingsOpen: false,
  phase: "Idle",
  tokenUsage: null,
  projectContext: null,
  kanbanBoard: null,
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
      const newActive =
        state.activeTabId === action.tabId
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
    case "TOGGLE_MARKETPLACE":
      return { ...state, marketplaceOpen: !state.marketplaceOpen, kanbanOpen: false };
    case "TOGGLE_GIT_PANEL":
      return { ...state, gitPanelOpen: !state.gitPanelOpen };
    case "TOGGLE_CHAT":
      return { ...state, chatOpen: !state.chatOpen };
    case "TOGGLE_KEYBINDING_SETTINGS":
      return { ...state, keybindingSettingsOpen: !state.keybindingSettingsOpen };
    case "OPEN_KEYBINDING_SETTINGS":
      return { ...state, keybindingSettingsOpen: true };
    case "CLOSE_KEYBINDING_SETTINGS":
      return { ...state, keybindingSettingsOpen: false };
    case "OPEN_KANBAN":
      return { ...state, kanbanOpen: true, marketplaceOpen: false, kanbanBoard: action.board };
    case "CLOSE_KANBAN":
      return { ...state, kanbanOpen: false };
    case "TOGGLE_KANBAN":
      return state.kanbanBoard
        ? { ...state, kanbanOpen: !state.kanbanOpen, marketplaceOpen: false }
        : state;
    case "SET_KANBAN_BOARD":
      return { ...state, kanbanBoard: action.board };
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
  const chatRef = useRef<ChatHandle>(null);
  const terminalRegionRef = useRef<HTMLDivElement>(null);

  const activeSession = state.sessions.find((s) => s.id === state.activeSessionId) ?? null;

  const keybindingActions = useMemo<Record<string, () => void>>(
    () => ({
      command_palette: () => dispatch({ type: "TOGGLE_COMMAND_PALETTE" }),
      new_terminal_tab: () => dispatch({ type: "NEW_TAB" }),
      close_tab: () => dispatch({ type: "CLOSE_TAB", tabId: state.activeTabId }),
      split_horizontal: () => {},
      split_vertical: () => {},
      toggle_chat: () => dispatch({ type: "TOGGLE_CHAT" }),
      toggle_git_panel: () => dispatch({ type: "TOGGLE_GIT_PANEL" }),
      toggle_marketplace: () => dispatch({ type: "TOGGLE_MARKETPLACE" }),
      toggle_kanban: () => dispatch({ type: "TOGGLE_KANBAN" }),
      send_message: () => chatRef.current?.sendMessage(),
      cancel_agent: () => {
        void chatRef.current?.cancelAgent();
      },
      focus_terminal: () => terminalRegionRef.current?.focus(),
      focus_chat: () => chatRef.current?.focusInput(),
      next_tab: () => {
        if (state.tabs.length <= 1) return;
        const currentIndex = state.tabs.findIndex((tab) => tab.id === state.activeTabId);
        const nextIndex = (currentIndex + 1) % state.tabs.length;
        dispatch({ type: "SET_ACTIVE_TAB", tabId: state.tabs[nextIndex].id });
      },
      prev_tab: () => {
        if (state.tabs.length <= 1) return;
        const currentIndex = state.tabs.findIndex((tab) => tab.id === state.activeTabId);
        const nextIndex = (currentIndex - 1 + state.tabs.length) % state.tabs.length;
        dispatch({ type: "SET_ACTIVE_TAB", tabId: state.tabs[nextIndex].id });
      },
      search_files: () => dispatch({ type: "TOGGLE_COMMAND_PALETTE" }),
      quick_open: () => dispatch({ type: "TOGGLE_COMMAND_PALETTE" }),
      settings: () => dispatch({ type: "OPEN_KEYBINDING_SETTINGS" }),
      switch_mode: () => dispatch({ type: "TOGGLE_MARKETPLACE" }),
      checkpoint: () => chatRef.current?.sendRaw("/checkpoint create"),
    }),
    [state.activeTabId, state.tabs]
  );

  const { reload } = useKeybindings(keybindingActions);

  const workspaceSubtitle = state.marketplaceOpen
    ? "Browse marketplace entries"
    : state.kanbanOpen
      ? "Kanban board"
      : "Terminal + project workspace";

  const workspaceButtonLabel = state.marketplaceOpen ? "Back to Terminal" : "Open Marketplace";
  const canOpenKanban = state.kanbanBoard !== null;

  const handleKanbanToggle = () => {
    if (state.kanbanOpen) {
      dispatch({ type: "CLOSE_KANBAN" });
    } else if (state.kanbanBoard) {
      dispatch({ type: "OPEN_KANBAN", board: state.kanbanBoard });
    }
  };

  const gridColumns = `${state.gitPanelOpen ? "240px" : "0px"} 1fr ${state.chatOpen ? "320px" : "0px"}`;

  return (
    <div className="app-layout" style={{ gridTemplateColumns: gridColumns }} tabIndex={-1}>
      {state.gitPanelOpen && (
        <aside className="sidebar">
          <GitPanel projectRoot={activeSession?.project_root ?? null} />
        </aside>
      )}

      <main className="main-area" ref={terminalRegionRef} tabIndex={-1}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "8px 12px",
            borderBottom: "1px solid #313244",
            background: "#181825",
            flexShrink: 0,
            gap: 8,
          }}
        >
          <div>
            <div style={{ fontSize: 12, fontWeight: 700 }}>Workspace</div>
            <div style={{ fontSize: 11, color: "#6c7086", marginTop: 2 }}>{workspaceSubtitle}</div>
          </div>

          <div style={{ display: "flex", gap: 8 }}>
            <button
              type="button"
              onClick={handleKanbanToggle}
              style={{
                border: "none",
                borderRadius: 8,
                padding: "8px 12px",
                background: state.kanbanOpen ? "#f9e2af" : "#94e2d5",
                color: "#1e1e2e",
                fontWeight: 700,
                cursor: "pointer",
              }}
              disabled={!canOpenKanban && !state.kanbanOpen}
            >
              {state.kanbanOpen ? "Back to Terminal" : "Open Kanban"}
            </button>
            <button
              type="button"
              onClick={() => dispatch({ type: "TOGGLE_MARKETPLACE" })}
              style={{
                border: "none",
                borderRadius: 8,
                padding: "8px 12px",
                background: state.marketplaceOpen ? "#cba6f7" : "#89b4fa",
                color: "#1e1e2e",
                fontWeight: 700,
                cursor: "pointer",
              }}
            >
              {workspaceButtonLabel}
            </button>
          </div>
        </div>

        {state.marketplaceOpen ? (
          <MarketplacePanel />
        ) : state.kanbanOpen ? (
          <KanbanBoard board={state.kanbanBoard} />
        ) : (
          <Terminal
            tabs={state.tabs}
            activeTabId={state.activeTabId}
            sessionId={state.activeSessionId}
            onTabChange={(id) => dispatch({ type: "SET_ACTIVE_TAB", tabId: id })}
            onTabClose={(id) => dispatch({ type: "CLOSE_TAB", tabId: id })}
            onNewTab={() => dispatch({ type: "NEW_TAB" })}
          />
        )}
      </main>

      {state.chatOpen && (
        <aside className="chat-pane">
          <Chat
            ref={chatRef}
            session={activeSession}
            onSessionCreated={(s) => dispatch({ type: "ADD_SESSION", session: s })}
            onPhaseChange={(phase) => dispatch({ type: "SET_PHASE", phase })}
            onTokenUsage={(usage) => dispatch({ type: "SET_TOKEN_USAGE", usage })}
            onOpenKanban={(board) => dispatch({ type: "OPEN_KANBAN", board })}
            onKanbanUpdated={(board) => dispatch({ type: "SET_KANBAN_BOARD", board })}
          />
        </aside>
      )}

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

      {state.keybindingSettingsOpen && (
        <KeybindingSettings
          onClose={() => dispatch({ type: "CLOSE_KEYBINDING_SETTINGS" })}
          onSaved={() => {
            void reload();
          }}
        />
      )}
    </div>
  );
}

export default App;
