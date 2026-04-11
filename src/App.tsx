import { useEffect, useMemo, useReducer, useRef, useState } from "react";
import Terminal from "./components/Terminal";
import Chat, { type ChatHandle } from "./components/Chat";
import GitPanel from "./components/GitPanel";
import StatusBar from "./components/StatusBar";
import CommandPalette from "./components/CommandPalette";
import MarketplacePanel from "./components/MarketplacePanel";
import KanbanBoard from "./components/KanbanBoard";
import KeybindingSettings from "./components/KeybindingSettings";
import ThemePicker from "./components/ThemePicker";
import SplitPane from "./components/SplitPane";
import AgentsWindow, { type AgentWindowTab } from "./components/AgentsWindow";
import DesktopNotifications from "./components/DesktopNotifications";
import { useKeybindings } from "./hooks/useKeybindings";
import { applyTheme, getTheme, loadStoredTheme, type ThemeName } from "./theme";
import type {
  KanbanBoard as KanbanBoardData,
  ProjectScanResult,
  SessionInfo,
  SessionPhase,
  TerminalTab,
  TokenUsage,
} from "./types";

interface AgentWorkspace {
  id: string;
  title: string;
  session: SessionInfo | null;
  terminalTabs: TerminalTab[];
  activeTerminalTabId: string;
  status: AgentWindowTab["status"];
}

interface AppState {
  workspaces: AgentWorkspace[];
  activeWorkspaceId: string;
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
  | { type: "ADD_WORKSPACE" }
  | { type: "CLOSE_WORKSPACE"; workspaceId: string }
  | { type: "RENAME_WORKSPACE"; workspaceId: string; title: string }
  | { type: "SET_ACTIVE_WORKSPACE"; workspaceId: string }
  | { type: "SET_WORKSPACE_SESSION"; workspaceId: string; session: SessionInfo }
  | { type: "SET_WORKSPACE_PHASE"; workspaceId: string; phase: SessionPhase }
  | { type: "NEW_TERMINAL_TAB"; workspaceId: string }
  | { type: "CLOSE_TERMINAL_TAB"; workspaceId: string; tabId: string }
  | { type: "SET_ACTIVE_TERMINAL_TAB"; workspaceId: string; tabId: string }
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

let workspaceCounter = 1;
let terminalCounter = 1;

function createTerminalTab(): TerminalTab {
  return { id: `terminal-${terminalCounter}`, title: `Terminal ${terminalCounter}` };
}

function createWorkspace(): AgentWorkspace {
  const terminalTab = createTerminalTab();
  const workspace = {
    id: `agent-${workspaceCounter}`,
    title: `Agent ${workspaceCounter}`,
    session: null,
    terminalTabs: [terminalTab],
    activeTerminalTabId: terminalTab.id,
    status: "idle" as const,
  };
  workspaceCounter += 1;
  terminalCounter += 1;
  return workspace;
}

const initialWorkspace = createWorkspace();

const initialState: AppState = {
  workspaces: [initialWorkspace],
  activeWorkspaceId: initialWorkspace.id,
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

function statusFromPhase(phase: SessionPhase): AgentWindowTab["status"] {
  switch (phase) {
    case "Running":
    case "AwaitingPermission":
      return "running";
    case "Error":
      return "error";
    case "Completed":
      return "complete";
    default:
      return "idle";
  }
}

function reducer(state: AppState, action: AppAction): AppState {
  switch (action.type) {
    case "ADD_WORKSPACE": {
      const workspace = createWorkspace();
      return {
        ...state,
        workspaces: [...state.workspaces, workspace],
        activeWorkspaceId: workspace.id,
      };
    }
    case "CLOSE_WORKSPACE": {
      if (state.workspaces.length === 1) {
        return state;
      }
      const remaining = state.workspaces.filter((workspace) => workspace.id !== action.workspaceId);
      const activeWorkspaceId =
        state.activeWorkspaceId === action.workspaceId ? remaining[remaining.length - 1].id : state.activeWorkspaceId;
      return { ...state, workspaces: remaining, activeWorkspaceId };
    }
    case "RENAME_WORKSPACE":
      return {
        ...state,
        workspaces: state.workspaces.map((workspace) =>
          workspace.id === action.workspaceId ? { ...workspace, title: action.title } : workspace
        ),
      };
    case "SET_ACTIVE_WORKSPACE":
      return { ...state, activeWorkspaceId: action.workspaceId };
    case "SET_WORKSPACE_SESSION":
      return {
        ...state,
        phase: action.session.phase,
        workspaces: state.workspaces.map((workspace) =>
          workspace.id === action.workspaceId
            ? {
                ...workspace,
                title: workspace.title.startsWith("Agent ")
                  ? action.session.project_root.split("/").pop() || workspace.title
                  : workspace.title,
                session: action.session,
                status: statusFromPhase(action.session.phase),
              }
            : workspace
        ),
      };
    case "SET_WORKSPACE_PHASE":
      return {
        ...state,
        phase: action.phase,
        workspaces: state.workspaces.map((workspace) =>
          workspace.id === action.workspaceId
            ? {
                ...workspace,
                status: statusFromPhase(action.phase),
                session: workspace.session ? { ...workspace.session, phase: action.phase } : null,
              }
            : workspace
        ),
      };
    case "NEW_TERMINAL_TAB": {
      terminalCounter += 1;
      const newTab: TerminalTab = { id: `terminal-${terminalCounter}`, title: `Terminal ${terminalCounter}` };
      return {
        ...state,
        workspaces: state.workspaces.map((workspace) =>
          workspace.id === action.workspaceId
            ? {
                ...workspace,
                terminalTabs: [...workspace.terminalTabs, newTab],
                activeTerminalTabId: newTab.id,
              }
            : workspace
        ),
      };
    }
    case "CLOSE_TERMINAL_TAB":
      return {
        ...state,
        workspaces: state.workspaces.map((workspace) => {
          if (workspace.id !== action.workspaceId || workspace.terminalTabs.length === 1) {
            return workspace;
          }
          const remaining = workspace.terminalTabs.filter((tab) => tab.id !== action.tabId);
          return {
            ...workspace,
            terminalTabs: remaining,
            activeTerminalTabId:
              workspace.activeTerminalTabId === action.tabId
                ? remaining[remaining.length - 1].id
                : workspace.activeTerminalTabId,
          };
        }),
      };
    case "SET_ACTIVE_TERMINAL_TAB":
      return {
        ...state,
        workspaces: state.workspaces.map((workspace) =>
          workspace.id === action.workspaceId
            ? { ...workspace, activeTerminalTabId: action.tabId }
            : workspace
        ),
      };
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
      return state.kanbanBoard ? { ...state, kanbanOpen: !state.kanbanOpen, marketplaceOpen: false } : state;
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
  const [themeName, setThemeName] = useState<ThemeName>(loadStoredTheme());
  const chatRef = useRef<ChatHandle>(null);
  const terminalRegionRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    applyTheme(themeName);
  }, [themeName]);

  const theme = useMemo(() => getTheme(themeName), [themeName]);
  const activeWorkspace = state.workspaces.find((workspace) => workspace.id === state.activeWorkspaceId) ?? state.workspaces[0];
  const activeSession = activeWorkspace?.session ?? null;

  const keybindingActions = useMemo<Record<string, () => void>>(
    () => ({
      command_palette: () => dispatch({ type: "TOGGLE_COMMAND_PALETTE" }),
      new_terminal_tab: () => dispatch({ type: "NEW_TERMINAL_TAB", workspaceId: activeWorkspace.id }),
      close_tab: () =>
        dispatch({
          type: "CLOSE_TERMINAL_TAB",
          workspaceId: activeWorkspace.id,
          tabId: activeWorkspace.activeTerminalTabId,
        }),
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
        if (activeWorkspace.terminalTabs.length <= 1) return;
        const currentIndex = activeWorkspace.terminalTabs.findIndex(
          (tab) => tab.id === activeWorkspace.activeTerminalTabId
        );
        const nextIndex = (currentIndex + 1) % activeWorkspace.terminalTabs.length;
        dispatch({
          type: "SET_ACTIVE_TERMINAL_TAB",
          workspaceId: activeWorkspace.id,
          tabId: activeWorkspace.terminalTabs[nextIndex].id,
        });
      },
      prev_tab: () => {
        if (activeWorkspace.terminalTabs.length <= 1) return;
        const currentIndex = activeWorkspace.terminalTabs.findIndex(
          (tab) => tab.id === activeWorkspace.activeTerminalTabId
        );
        const nextIndex = (currentIndex - 1 + activeWorkspace.terminalTabs.length) % activeWorkspace.terminalTabs.length;
        dispatch({
          type: "SET_ACTIVE_TERMINAL_TAB",
          workspaceId: activeWorkspace.id,
          tabId: activeWorkspace.terminalTabs[nextIndex].id,
        });
      },
      search_files: () => dispatch({ type: "TOGGLE_COMMAND_PALETTE" }),
      quick_open: () => dispatch({ type: "TOGGLE_COMMAND_PALETTE" }),
      settings: () => dispatch({ type: "OPEN_KEYBINDING_SETTINGS" }),
      switch_mode: () => dispatch({ type: "TOGGLE_MARKETPLACE" }),
      checkpoint: () => chatRef.current?.sendRaw("/checkpoint create"),
    }),
    [activeWorkspace]
  );

  const { reload } = useKeybindings(keybindingActions);

  const workspaceSubtitle = state.marketplaceOpen
    ? "Browse marketplace entries"
    : state.kanbanOpen
      ? "Kanban board"
      : "Parallel agent tabs with independent terminals and chat";

  const workspaceTabs = state.workspaces.map<AgentWindowTab>((workspace) => ({
    id: workspace.id,
    title: workspace.title,
    status: workspace.status,
    subtitle: workspace.session?.model_id ?? "New session",
  }));

  const terminalPanels = state.workspaces.map((workspace) => (
    <div
      key={workspace.id}
      style={{
        display: workspace.id === activeWorkspace.id ? "flex" : "none",
        flex: 1,
        minHeight: 0,
      }}
    >
      <Terminal
        tabs={workspace.terminalTabs}
        activeTabId={workspace.activeTerminalTabId}
        onTabChange={(tabId) => dispatch({ type: "SET_ACTIVE_TERMINAL_TAB", workspaceId: workspace.id, tabId })}
        onTabClose={(tabId) => dispatch({ type: "CLOSE_TERMINAL_TAB", workspaceId: workspace.id, tabId })}
        onNewTab={() => dispatch({ type: "NEW_TERMINAL_TAB", workspaceId: workspace.id })}
        terminalTheme={theme.terminal}
      />
    </div>
  ));

  const workspaceContent = state.marketplaceOpen ? (
    <MarketplacePanel />
  ) : state.kanbanOpen ? (
    <KanbanBoard board={state.kanbanBoard} />
  ) : (
    terminalPanels
  );

  const centerPane = (
    <main className="workspace-main" ref={terminalRegionRef} tabIndex={-1}>
      <div className="workspace-toolbar">
        <div>
          <div className="workspace-title">Workspace</div>
          <div className="workspace-subtitle">{workspaceSubtitle}</div>
        </div>
        <div className="workspace-toolbar__controls">
          <ThemePicker value={themeName} onChange={setThemeName} />
          <button type="button" className="workspace-button" onClick={() => dispatch({ type: "TOGGLE_KANBAN" })}>
            {state.kanbanOpen ? "Back to terminal" : "Open kanban"}
          </button>
          <button
            type="button"
            className="workspace-button workspace-button--accent"
            onClick={() => dispatch({ type: "TOGGLE_MARKETPLACE" })}
          >
            {state.marketplaceOpen ? "Back to terminal" : "Open marketplace"}
          </button>
        </div>
      </div>

      <AgentsWindow
        tabs={workspaceTabs}
        activeTabId={activeWorkspace.id}
        onSelect={(workspaceId) => dispatch({ type: "SET_ACTIVE_WORKSPACE", workspaceId })}
        onAdd={() => dispatch({ type: "ADD_WORKSPACE" })}
        onClose={(workspaceId) => dispatch({ type: "CLOSE_WORKSPACE", workspaceId })}
        onRename={(workspaceId) => {
          const workspace = state.workspaces.find((item) => item.id === workspaceId);
          const title = window.prompt("Rename agent tab", workspace?.title ?? "");
          if (title?.trim()) {
            dispatch({ type: "RENAME_WORKSPACE", workspaceId, title: title.trim() });
          }
        }}
      />

      <div className="workspace-stage">{workspaceContent}</div>
    </main>
  );

  const rightPane = state.chatOpen ? (
    <aside className="chat-pane">
      <Chat
        key={activeWorkspace.id}
        ref={chatRef}
        session={activeSession}
        onSessionCreated={(session) => dispatch({ type: "SET_WORKSPACE_SESSION", workspaceId: activeWorkspace.id, session })}
        onSessionUpdated={(session) => dispatch({ type: "SET_WORKSPACE_SESSION", workspaceId: activeWorkspace.id, session })}
        onPhaseChange={(phase) => dispatch({ type: "SET_WORKSPACE_PHASE", workspaceId: activeWorkspace.id, phase })}
        onTokenUsage={(usage) => dispatch({ type: "SET_TOKEN_USAGE", usage })}
      />
    </aside>
  ) : null;

  const centerAndRight = state.chatOpen ? (
    <SplitPane direction="horizontal" storageKey="caduceus:layout:center-chat" defaultSplit={0.68}>
      {centerPane}
      {rightPane}
    </SplitPane>
  ) : (
    centerPane
  );

  const shellContent = state.gitPanelOpen ? (
    <SplitPane direction="horizontal" storageKey="caduceus:layout:git-main" defaultSplit={0.2} minSize={0.12}>
      <aside className="sidebar">
        <GitPanel projectRoot={activeSession?.project_root ?? null} />
      </aside>
      {centerAndRight}
    </SplitPane>
  ) : (
    centerAndRight
  );

  return (
    <div className="app-layout">
      <DesktopNotifications />
      <div className="app-layout__content">{shellContent}</div>

      <StatusBar
        session={activeSession}
        phase={state.phase}
        tokenUsage={state.tokenUsage}
        projectContext={state.projectContext}
      />

      {state.commandPaletteOpen ? (
        <CommandPalette
          onClose={() => dispatch({ type: "CLOSE_COMMAND_PALETTE" })}
          onSessionSelect={(session) => {
            dispatch({ type: "SET_WORKSPACE_SESSION", workspaceId: activeWorkspace.id, session });
            dispatch({ type: "CLOSE_COMMAND_PALETTE" });
          }}
        />
      ) : null}

      {state.keybindingSettingsOpen ? (
        <KeybindingSettings
          onClose={() => dispatch({ type: "CLOSE_KEYBINDING_SETTINGS" })}
          onSaved={() => {
            void reload();
          }}
        />
      ) : null}
    </div>
  );
}

export default App;
