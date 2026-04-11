import { useEffect, useMemo, useReducer, useRef, useState } from "react";
import Terminal from "./components/Terminal";
import Chat, { type ChatHandle } from "./components/Chat";
import GitPanel from "./components/GitPanel";
import StatusBar from "./components/StatusBar";
import CommandPalette from "./components/CommandPalette";
import BuddySprite from "./components/BuddySprite";
import CardTerminal from "./components/CardTerminal";
import ContextQualityBadge from "./components/ContextQualityBadge";
import ContextViewer from "./components/ContextViewer";
import ContextVisualizer from "./components/ContextVisualizer";
import DesignMode from "./components/DesignMode";
import DiffViewer from "./components/DiffViewer";
import ImageRenderer from "./components/ImageRenderer";
import InlineDiffReview from "./components/InlineDiffReview";
import MarketplacePanel from "./components/MarketplacePanel";
import MarketplaceCard from "./components/MarketplaceCard";
import ModelPicker from "./components/ModelPicker";
import KanbanBoard from "./components/KanbanBoard";
import KeybindingSettings from "./components/KeybindingSettings";
import SessionBrowser from "./components/SessionBrowser";
import SyntaxHighlighter from "./components/SyntaxHighlighter";
import ThemePicker from "./components/ThemePicker";
import SplitPane from "./components/SplitPane";
import AgentsWindow, { type AgentWindowTab } from "./components/AgentsWindow";
import DesktopNotifications from "./components/DesktopNotifications";
import VimMode from "./components/VimMode";
import VoiceInput from "./components/VoiceInput";
import OnboardingTour, { HelpPanel, ContextualHelpOverlay } from "./components/OnboardingTour";
import { useKeybindings } from "./hooks/useKeybindings";
import { useOnboarding } from "./hooks/useOnboarding";
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

interface DesignAnnotation {
  id: string;
  type: "arrow" | "box" | "text" | "highlight";
  x: number;
  y: number;
  width?: number;
  height?: number;
  text?: string;
  color: string;
}

interface AppState {
  workspaces: AgentWorkspace[];
  activeWorkspaceId: string;
  commandPaletteOpen: boolean;
  marketplaceOpen: boolean;
  kanbanOpen: boolean;
  contextViewerOpen: boolean;
  sessionBrowserOpen: boolean;
  diffViewerOpen: boolean;
  designModeEnabled: boolean;
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
  | { type: "TOGGLE_CONTEXT_VIEWER" }
  | { type: "TOGGLE_SESSION_BROWSER" }
  | { type: "TOGGLE_DIFF_VIEWER" }
  | { type: "TOGGLE_DESIGN_MODE" }
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
  contextViewerOpen: false,
  sessionBrowserOpen: false,
  diffViewerOpen: false,
  designModeEnabled: false,
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
    case "TOGGLE_CONTEXT_VIEWER":
      return { ...state, contextViewerOpen: !state.contextViewerOpen };
    case "TOGGLE_SESSION_BROWSER":
      return { ...state, sessionBrowserOpen: !state.sessionBrowserOpen };
    case "TOGGLE_DIFF_VIEWER":
      return { ...state, diffViewerOpen: !state.diffViewerOpen };
    case "TOGGLE_DESIGN_MODE":
      return { ...state, designModeEnabled: !state.designModeEnabled };
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
  const [selectedModelId, setSelectedModelId] = useState("claude-sonnet-4-5");
  const [designAnnotations, setDesignAnnotations] = useState<DesignAnnotation[]>([]);
  const { helpOpen, setHelpOpen, helpOverlayOpen, setHelpOverlayOpen } = useOnboarding();
  const chatRef = useRef<ChatHandle>(null);
  const terminalRegionRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    applyTheme(themeName);
  }, [themeName]);

  const theme = useMemo(() => getTheme(themeName), [themeName]);
  const activeWorkspace = state.workspaces.find((workspace) => workspace.id === state.activeWorkspaceId) ?? state.workspaces[0];
  const activeSession = activeWorkspace?.session ?? null;

  useEffect(() => {
    if (activeSession?.model_id) {
      setSelectedModelId(activeSession.model_id);
    }
  }, [activeSession?.model_id]);

  const sessionEntries = useMemo(
    () =>
      state.workspaces
        .filter((workspace): workspace is AgentWorkspace & { session: SessionInfo } => workspace.session !== null)
        .map((workspace, index) => {
          const parsedDate = Date.parse(workspace.session.id);
          return {
            id: workspace.session.id,
            date: Number.isNaN(parsedDate)
              ? new Date(Date.now() - index * 60_000).toISOString()
              : new Date(parsedDate).toISOString(),
            firstMessage: workspace.title,
            tokenCount:
              workspace.session.token_budget.used_input +
              workspace.session.token_budget.used_output +
              workspace.session.token_budget.reserved_output,
            modelId: workspace.session.model_id,
          };
        }),
    [state.workspaces]
  );
  const contextScore = activeSession
    ? Math.max(
        0,
        1 -
          (activeSession.token_budget.used_input +
            activeSession.token_budget.used_output +
            activeSession.token_budget.reserved_output) /
            Math.max(activeSession.token_budget.context_limit, 1)
      )
    : 0.85;
  const buddyMood =
    state.phase === "Running" || state.phase === "AwaitingPermission"
      ? "working"
      : state.phase === "Error"
        ? "error"
        : state.phase === "Completed"
          ? "celebrating"
          : "idle";

  // Child components used within other components:
  // - SyntaxHighlighter: used in Chat for code blocks
  // - MarketplaceCard: used in MarketplacePanel for skill cards
  // - InlineDiffReview: used in DiffViewer for diff display
  // - ImageRenderer: used in Chat for image messages
  // - CardTerminal: used in KanbanBoard for per-card previews
  // - VimMode: used in Chat input when vim mode enabled
  // - ContextVisualizer: used in StatusBar for token breakdown

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
          <ModelPicker value={selectedModelId} onChange={setSelectedModelId} />
          <ThemePicker value={themeName} onChange={setThemeName} />
          <button
            type="button"
            className="workspace-button"
            onClick={() => dispatch({ type: "TOGGLE_CONTEXT_VIEWER" })}
          >
            {state.contextViewerOpen ? "Hide context" : "Open context"}
          </button>
          <button
            type="button"
            className="workspace-button"
            onClick={() => dispatch({ type: "TOGGLE_SESSION_BROWSER" })}
          >
            {state.sessionBrowserOpen ? "Hide sessions" : "Open sessions"}
          </button>
          <button
            type="button"
            className="workspace-button"
            onClick={() => dispatch({ type: "TOGGLE_DIFF_VIEWER" })}
          >
            {state.diffViewerOpen ? "Hide diff" : "Open diff"}
          </button>
          <button
            type="button"
            className="workspace-button"
            onClick={() => dispatch({ type: "TOGGLE_DESIGN_MODE" })}
          >
            {state.designModeEnabled ? "Disable design mode" : "Enable design mode"}
          </button>
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

      {state.contextViewerOpen && activeSession ? (
        <div style={{ padding: "0 20px 20px" }}>
          <ContextViewer tokenBudget={activeSession.token_budget} />
        </div>
      ) : null}

      {state.sessionBrowserOpen ? (
        <div style={{ padding: "0 20px 20px" }}>
          <SessionBrowser
            sessions={sessionEntries}
            activeSessionId={activeSession?.id}
            onResume={(sessionId) => {
              const workspace = state.workspaces.find((item) => item.session?.id === sessionId);
              if (workspace) {
                dispatch({ type: "SET_ACTIVE_WORKSPACE", workspaceId: workspace.id });
              }
            }}
            onDelete={(sessionId) => {
              const workspace = state.workspaces.find((item) => item.session?.id === sessionId);
              if (workspace) {
                dispatch({ type: "CLOSE_WORKSPACE", workspaceId: workspace.id });
              }
            }}
          />
        </div>
      ) : null}

      {state.diffViewerOpen ? (
        <div style={{ padding: "0 20px 20px" }}>
          <DiffViewer diffs={[]} />
        </div>
      ) : null}
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
      <div style={{ padding: "0 16px 16px" }}>
        <VoiceInput onTranscript={(text) => chatRef.current?.sendRaw(text)} enabled={true} />
      </div>
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
      <div className="app-layout__content" style={{ position: "relative" }}>
        {shellContent}
        <DesignMode
          enabled={state.designModeEnabled}
          annotations={designAnnotations}
          onAddAnnotation={(annotation) => {
            setDesignAnnotations((current) => [...current, annotation]);
          }}
          onRemoveAnnotation={(annotationId) => {
            setDesignAnnotations((current) => current.filter((annotation) => annotation.id !== annotationId));
          }}
        />
      </div>

      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 12,
          padding: "8px 16px 0",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <BuddySprite mood={buddyMood} size={32} />
          <ContextQualityBadge score={contextScore} />
        </div>
      </div>

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

      <OnboardingTour />
      <HelpPanel
        open={helpOpen}
        onClose={() => setHelpOpen(false)}
      />
      <ContextualHelpOverlay
        enabled={helpOverlayOpen}
        onClose={() => setHelpOverlayOpen(false)}
      />
    </div>
  );
}

export default App;
