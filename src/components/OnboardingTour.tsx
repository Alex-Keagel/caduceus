import React, { useState, useEffect, useCallback } from "react";
import {
  Joyride,
  type Step,
  type EventData,
  type Controls,
  STATUS,
  ACTIONS,
  EVENTS,
} from "react-joyride";

const ONBOARDING_KEY = "caduceus_onboarding_complete";
const FEATURE_DISCOVERY_KEY = "caduceus_features_discovered";

const tourSteps: Step[] = [
  {
    target: "body",
    content:
      "Welcome to Caduceus! 🐍 Your AI-powered development environment. Let me show you around.",
    placement: "center",
    skipBeacon: true,
  },
  {
    target: '[data-tour="terminal"]',
    content:
      "This is your Terminal — run commands, see output, and interact with your system. Caduceus wraps a full PTY so everything works just like your regular terminal.",
    skipBeacon: true,
  },
  {
    target: '[data-tour="chat"]',
    content:
      "The AI Chat panel — talk to your AI agent here. Use slash commands like /help, /model, /compact, /config. The agent can read files, edit code, run tests, and more.",
    skipBeacon: true,
  },
  {
    target: '[data-tour="git-panel"]',
    content:
      "Git Panel — see your repository status, staged changes, and diffs at a glance. The agent can also manage git operations for you.",
    skipBeacon: true,
  },
  {
    target: '[data-tour="kanban"]',
    content:
      "Kanban Board — organize tasks into cards. Each card can have its own agent running in the background with isolated git worktrees.",
    skipBeacon: true,
  },
  {
    target: '[data-tour="marketplace"]',
    content:
      "Marketplace — browse and install skills, agents, and MCP servers. Skills evolve automatically from your usage patterns.",
    skipBeacon: true,
  },
  {
    target: '[data-tour="agents-window"]',
    content:
      "Agents Window — run multiple AI agents in parallel tabs. Monitor, approve, or cancel each independently.",
    skipBeacon: true,
  },
  {
    target: '[data-tour="status-bar"]',
    content:
      "Status Bar — shows your current model, token usage, git branch, cost, and context health. The context quality badge warns you before context rot degrades performance.",
    skipBeacon: true,
  },
  {
    target: '[data-tour="command-palette"]',
    content:
      "Command Palette (Ctrl/⌘+Shift+P) — quick access to all commands, settings, and features. Customize keybindings to match your favorite IDE.",
    skipBeacon: true,
  },
  {
    target: "body",
    content:
      "You're all set! 🎉 Use /help in the chat to see all available commands. Press ? at any time for contextual help. Happy coding!",
    placement: "center",
    skipBeacon: true,
  },
];

interface OnboardingTourProps {
  enabled?: boolean;
  onComplete?: () => void;
}

export default function OnboardingTour({
  enabled,
  onComplete,
}: OnboardingTourProps) {
  const [run, setRun] = useState(false);

  useEffect(() => {
    if (enabled !== undefined) {
      setRun(enabled);
      return;
    }
    const completed = localStorage.getItem(ONBOARDING_KEY);
    if (!completed) {
      const timer = setTimeout(() => setRun(true), 1000);
      return () => clearTimeout(timer);
    }
  }, [enabled]);

  const handleEvent = useCallback(
    (data: EventData, controls: Controls) => {
      const { status, action, index, type } = data;

      if (type === EVENTS.STEP_AFTER || type === EVENTS.TARGET_NOT_FOUND) {
        if (action === ACTIONS.PREV) {
          controls.prev();
        } else {
          controls.next();
        }
      }

      if (
        status === STATUS.FINISHED ||
        status === STATUS.SKIPPED
      ) {
        setRun(false);
        localStorage.setItem(ONBOARDING_KEY, "true");
        onComplete?.();
      }
    },
    [onComplete]
  );

  return (
    <Joyride
      steps={tourSteps}
      run={run}
      continuous
      onEvent={handleEvent}
      options={{
        buttons: ["back", "primary", "skip"],
        showProgress: true,
        primaryColor: "#6366f1",
        backgroundColor: "#1e1e2e",
        textColor: "#cdd6f4",
        overlayColor: "rgba(0, 0, 0, 0.7)",
        arrowColor: "#1e1e2e",
        zIndex: 10000,
      }}
      locale={{
        back: "Back",
        close: "Close",
        last: "Let's Go!",
        next: "Next",
        skip: "Skip Tour",
      }}
    />
  );
}

// --- Feature Discovery ---

interface FeatureHint {
  id: string;
  title: string;
  description: string;
  icon: string;
}

const featureHints: Record<string, FeatureHint> = {
  kanban: {
    id: "kanban",
    title: "Kanban Board",
    icon: "📋",
    description:
      "Organize work into cards with AI agents. Each card gets its own terminal and can run tasks independently.",
  },
  marketplace: {
    id: "marketplace",
    title: "Marketplace",
    icon: "🏪",
    description:
      "Install skills and agents to extend Caduceus. Skills auto-evolve from your usage patterns!",
  },
  git: {
    id: "git",
    title: "Git Integration",
    icon: "🔀",
    description:
      "Full git status, diffs, stale-base detection, and auto-commit. The agent understands your repo.",
  },
  agents: {
    id: "agents",
    title: "Multi-Agent",
    icon: "🤖",
    description:
      "Run multiple agents in parallel tabs — each with its own context, tools, and task.",
  },
  keybindings: {
    id: "keybindings",
    title: "Keybindings",
    icon: "⌨️",
    description:
      "Customize shortcuts with IDE presets: IntelliJ, VS Code, Vim, or Emacs.",
  },
  security: {
    id: "security",
    title: "Security Scanner",
    icon: "🔒",
    description:
      "Built-in SAST scanner detects secrets, injection flaws, weak crypto, and PII leaks in your code.",
  },
  context: {
    id: "context",
    title: "Context Management",
    icon: "🧠",
    description:
      "Automatic compaction, self-eviction, and attention tracking keep your AI focused over long sessions.",
  },
};

interface FeatureDiscoveryProps {
  featureId: string;
  children: React.ReactNode;
}

export function FeatureDiscovery({
  featureId,
  children,
}: FeatureDiscoveryProps) {
  const [show, setShow] = useState(false);
  const hint = featureHints[featureId];

  useEffect(() => {
    if (!hint) return;
    const discovered = JSON.parse(
      localStorage.getItem(FEATURE_DISCOVERY_KEY) || "{}"
    );
    if (!discovered[featureId]) {
      setShow(true);
    }
  }, [featureId, hint]);

  const dismiss = useCallback(() => {
    setShow(false);
    const discovered = JSON.parse(
      localStorage.getItem(FEATURE_DISCOVERY_KEY) || "{}"
    );
    discovered[featureId] = true;
    localStorage.setItem(FEATURE_DISCOVERY_KEY, JSON.stringify(discovered));
  }, [featureId]);

  if (!show || !hint) return <>{children}</>;

  return (
    <div style={{ position: "relative" }}>
      {children}
      <div
        style={{
          position: "absolute",
          top: 4,
          right: 4,
          background: "#1e1e2e",
          border: "1px solid #6366f1",
          borderRadius: 10,
          padding: "12px 16px",
          maxWidth: 280,
          zIndex: 9999,
          boxShadow: "0 4px 20px rgba(99, 102, 241, 0.3)",
        }}
      >
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            marginBottom: 6,
          }}
        >
          <strong style={{ color: "#cdd6f4", fontSize: 14 }}>
            {hint.icon} {hint.title}
          </strong>
          <button
            onClick={dismiss}
            style={{
              background: "none",
              border: "none",
              color: "#6c7086",
              cursor: "pointer",
              fontSize: 16,
              padding: 0,
            }}
          >
            ✕
          </button>
        </div>
        <p style={{ color: "#a6adc8", fontSize: 13, margin: 0, lineHeight: 1.4 }}>
          {hint.description}
        </p>
      </div>
    </div>
  );
}

// --- Help Panel ---

interface CommandEntry {
  command: string;
  description: string;
  category: string;
  example?: string;
}

const commandReference: CommandEntry[] = [
  { command: "/help", description: "Show this help panel", category: "General" },
  { command: "/clear", description: "Clear conversation history", category: "General" },
  { command: "/compact", description: "Compress context window to free space", category: "Context" },
  { command: "/context", description: "Show context usage breakdown", category: "Context" },
  { command: "/checkpoint", description: "Save verified facts for later resume", category: "Context" },
  { command: "/purge", description: "Remove old context before a checkpoint", category: "Context" },
  { command: "/model", description: "Switch AI model at runtime", category: "Model", example: "/model claude-sonnet-4.6" },
  { command: "/config", description: "Get or set configuration values", category: "Config", example: "/config provider.model gpt-5.4" },
  { command: "/init", description: "Initialize Caduceus project files", category: "Project" },
  { command: "/export", description: "Export conversation to markdown", category: "Session" },
  { command: "/fork", description: "Fork current session to explore alternatives", category: "Session" },
  { command: "/mode", description: "Switch agent mode (plan/act/research/autopilot/architect/debug/review)", category: "Agent", example: "/mode autopilot" },
  { command: "/agent", description: "Switch to a specific agent persona", category: "Agent" },
  { command: "/kanban", description: "Open kanban board", category: "UI" },
  { command: "/marketplace", description: "Open skill marketplace", category: "UI" },
  { command: "/git", description: "Open git panel", category: "UI" },
  { command: "/security", description: "Run security scan on project", category: "Security" },
  { command: "/scan-deps", description: "Scan dependencies for vulnerabilities", category: "Security" },
  { command: "@file", description: "Add file to context", category: "Mentions", example: "@file src/main.rs" },
  { command: "@folder", description: "Add folder to context", category: "Mentions", example: "@folder src/" },
  { command: "@url", description: "Fetch and add URL content", category: "Mentions", example: "@url https://docs.rs" },
  { command: "@git", description: "Add git diff/log to context", category: "Mentions", example: "@git diff" },
  { command: "Ctrl+Shift+P", description: "Open command palette", category: "Keybinding" },
  { command: "Ctrl+`", description: "Toggle terminal", category: "Keybinding" },
  { command: "Ctrl+Shift+G", description: "Toggle git panel", category: "Keybinding" },
  { command: "Ctrl+Shift+K", description: "Toggle kanban", category: "Keybinding" },
  { command: "Ctrl+Shift+M", description: "Toggle marketplace", category: "Keybinding" },
  { command: "?", description: "Toggle contextual help overlay", category: "Keybinding" },
];

interface HelpPanelProps {
  open: boolean;
  onClose: () => void;
}

export function HelpPanel({ open, onClose }: HelpPanelProps) {
  const [search, setSearch] = useState("");

  if (!open) return null;

  const filtered = commandReference.filter(
    (cmd) =>
      cmd.command.toLowerCase().includes(search.toLowerCase()) ||
      cmd.description.toLowerCase().includes(search.toLowerCase()) ||
      cmd.category.toLowerCase().includes(search.toLowerCase())
  );

  const categories = [...new Set(filtered.map((c) => c.category))];

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.6)",
        zIndex: 10001,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
      onClick={onClose}
    >
      <div
        style={{
          background: "#1e1e2e",
          border: "1px solid #313244",
          borderRadius: 16,
          width: 600,
          maxHeight: "80vh",
          overflow: "hidden",
          display: "flex",
          flexDirection: "column",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div style={{ padding: "16px 20px", borderBottom: "1px solid #313244" }}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 12 }}>
            <h2 style={{ margin: 0, color: "#cdd6f4", fontSize: 18 }}>
              📖 Command Reference
            </h2>
            <button
              onClick={onClose}
              style={{ background: "none", border: "none", color: "#6c7086", cursor: "pointer", fontSize: 20 }}
            >
              ✕
            </button>
          </div>
          <input
            type="text"
            placeholder="Search commands, keybindings, mentions..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            autoFocus
            style={{
              width: "100%",
              padding: "10px 14px",
              background: "#11111b",
              border: "1px solid #313244",
              borderRadius: 8,
              color: "#cdd6f4",
              fontSize: 14,
              outline: "none",
              boxSizing: "border-box",
            }}
          />
        </div>
        <div style={{ overflow: "auto", padding: "12px 20px" }}>
          {categories.map((cat) => (
            <div key={cat} style={{ marginBottom: 16 }}>
              <h3 style={{ color: "#6366f1", fontSize: 12, textTransform: "uppercase", letterSpacing: 1, margin: "0 0 8px" }}>
                {cat}
              </h3>
              {filtered
                .filter((c) => c.category === cat)
                .map((cmd) => (
                  <div
                    key={cmd.command}
                    style={{
                      display: "flex",
                      justifyContent: "space-between",
                      alignItems: "flex-start",
                      padding: "6px 0",
                      borderBottom: "1px solid #181825",
                    }}
                  >
                    <div>
                      <code
                        style={{
                          color: "#89b4fa",
                          background: "#11111b",
                          padding: "2px 8px",
                          borderRadius: 4,
                          fontSize: 13,
                        }}
                      >
                        {cmd.command}
                      </code>
                      {cmd.example && (
                        <span style={{ color: "#585b70", fontSize: 12, marginLeft: 8 }}>
                          e.g. {cmd.example}
                        </span>
                      )}
                    </div>
                    <span style={{ color: "#a6adc8", fontSize: 13, textAlign: "right", maxWidth: 280 }}>
                      {cmd.description}
                    </span>
                  </div>
                ))}
            </div>
          ))}
          {filtered.length === 0 && (
            <p style={{ color: "#6c7086", textAlign: "center", padding: 20 }}>
              No commands matching "{search}"
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

// --- Contextual Help Overlay ---

interface HelpBadge {
  selector: string;
  label: string;
}

const helpBadges: HelpBadge[] = [
  { selector: '[data-tour="terminal"]', label: "Terminal — run commands & see output" },
  { selector: '[data-tour="chat"]', label: "AI Chat — talk to your agent" },
  { selector: '[data-tour="git-panel"]', label: "Git — status, diffs, branches" },
  { selector: '[data-tour="kanban"]', label: "Kanban — task management" },
  { selector: '[data-tour="marketplace"]', label: "Marketplace — skills & agents" },
  { selector: '[data-tour="status-bar"]', label: "Status — model, tokens, context health" },
  { selector: '[data-tour="agents-window"]', label: "Agents — parallel AI tabs" },
  { selector: '[data-tour="command-palette"]', label: "Commands — Ctrl+Shift+P" },
];

interface ContextualHelpOverlayProps {
  enabled: boolean;
  onClose: () => void;
}

export function ContextualHelpOverlay({ enabled, onClose }: ContextualHelpOverlayProps) {
  const [badges, setBadges] = useState<{ label: string; x: number; y: number }[]>([]);

  useEffect(() => {
    if (!enabled) {
      setBadges([]);
      return;
    }

    const computed: { label: string; x: number; y: number }[] = [];
    for (const badge of helpBadges) {
      const el = document.querySelector(badge.selector);
      if (el) {
        const rect = el.getBoundingClientRect();
        computed.push({
          label: badge.label,
          x: rect.left + rect.width / 2,
          y: rect.top + rect.height / 2,
        });
      }
    }
    setBadges(computed);
  }, [enabled]);

  useEffect(() => {
    if (!enabled) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" || e.key === "?") {
        onClose();
      }
    };
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [enabled, onClose]);

  if (!enabled) return null;

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.5)",
        zIndex: 10002,
        cursor: "pointer",
      }}
      onClick={onClose}
    >
      <div
        style={{
          position: "absolute",
          top: 16,
          left: "50%",
          transform: "translateX(-50%)",
          background: "#6366f1",
          color: "#fff",
          padding: "8px 16px",
          borderRadius: 8,
          fontSize: 14,
          fontWeight: 600,
        }}
      >
        Press ? or Esc to close help overlay
      </div>
      {badges.map((badge, i) => (
        <div
          key={i}
          style={{
            position: "absolute",
            left: badge.x,
            top: badge.y,
            transform: "translate(-50%, -50%)",
            background: "#1e1e2e",
            border: "2px solid #6366f1",
            borderRadius: 8,
            padding: "6px 12px",
            color: "#cdd6f4",
            fontSize: 13,
            whiteSpace: "nowrap",
            boxShadow: "0 2px 12px rgba(99,102,241,0.4)",
            pointerEvents: "none",
          }}
        >
          {badge.label}
        </div>
      ))}
    </div>
  );
}
