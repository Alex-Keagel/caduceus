import { useState, useRef, useEffect } from "react";
import { agentTurn, kanbanAddCard, kanbanLoad, permissionRespond, sessionCreate } from "../api/tauri";
import { listenAgentEvent } from "../api/tauri";
import type { KanbanBoard, SessionInfo, SessionPhase, TokenUsage, ChatMessage, ToolCallBlock, PermissionRequest } from "../types";
import type { UnlistenFn } from "@tauri-apps/api/event";

interface ChatProps {
  session: SessionInfo | null;
  onSessionCreated: (session: SessionInfo) => void;
  onPhaseChange?: (phase: SessionPhase) => void;
  onTokenUsage?: (usage: TokenUsage) => void;
  onOpenKanban?: (board: KanbanBoard) => void;
  onKanbanUpdated?: (board: KanbanBoard) => void;
}

const RISK_COLORS: Record<string, string> = {
  Low: "#a6e3a1",
  Medium: "#f9e2af",
  High: "#fab387",
  Critical: "#f38ba8",
};

function renderMarkdown(text: string): React.ReactNode {
  if (!text) return null;
  const parts = text.split(/(```[\w]*\n[\s\S]*?```|`[^`\n]+`)/g);
  return (
    <>
      {parts.map((part, i) => {
        if (part.startsWith("```")) {
          const firstNewline = part.indexOf("\n");
          const code =
            firstNewline >= 0
              ? part.slice(firstNewline + 1).replace(/```$/, "")
              : part.slice(3).replace(/```$/, "");
          return (
            <pre
              key={i}
              style={{
                background: "#11111b",
                border: "1px solid #313244",
                borderRadius: 6,
                padding: "8px 12px",
                overflow: "auto",
                margin: "8px 0",
                fontSize: 11,
                fontFamily: '"JetBrains Mono", monospace',
                whiteSpace: "pre",
              }}
            >
              <code>{code}</code>
            </pre>
          );
        }
        if (part.startsWith("`") && part.endsWith("`")) {
          return (
            <code
              key={i}
              style={{
                background: "#313244",
                padding: "1px 4px",
                borderRadius: 3,
                fontFamily: '"JetBrains Mono", monospace',
                fontSize: 11,
              }}
            >
              {part.slice(1, -1)}
            </code>
          );
        }
        return (
          <span key={i} style={{ whiteSpace: "pre-wrap" }}>
            {part}
          </span>
        );
      })}
    </>
  );
}

function ToolCallView({ tc, onToggle }: { tc: ToolCallBlock; onToggle: () => void }) {
  const resultColor = tc.is_error ? "#f38ba8" : "#a6e3a1";
  return (
    <div
      style={{
        margin: "4px 0",
        border: "1px solid #313244",
        borderRadius: 6,
        overflow: "hidden",
        fontSize: 11,
      }}
    >
      <div
        onClick={onToggle}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 6,
          padding: "4px 8px",
          background: "#181825",
          cursor: "pointer",
          userSelect: "none",
        }}
      >
        <span style={{ color: "#89b4fa" }}>⚙ {tc.name}</span>
        {tc.result !== undefined && (
          <span style={{ color: resultColor, marginLeft: "auto" }}>
            {tc.is_error ? "✗" : "✓"}
          </span>
        )}
        <span style={{ color: "#6c7086" }}>{tc.collapsed ? "▶" : "▼"}</span>
      </div>
      {!tc.collapsed && (
        <div style={{ padding: "6px 8px", background: "#11111b" }}>
          {tc.input_json && (
            <pre
              style={{
                margin: 0,
                fontSize: 10,
                color: "#cba6f7",
                overflow: "auto",
                maxHeight: 120,
              }}
            >
              {tc.input_json}
            </pre>
          )}
          {tc.result !== undefined && (
            <pre
              style={{
                margin: "4px 0 0",
                fontSize: 10,
                color: resultColor,
                overflow: "auto",
                maxHeight: 120,
              }}
            >
              {tc.result}
            </pre>
          )}
        </div>
      )}
    </div>
  );
}

export default function Chat({ session, onSessionCreated, onPhaseChange, onTokenUsage, onOpenKanban, onKanbanUpdated }: ChatProps) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [loading, setLoading] = useState(false);
  const [projectRoot, setProjectRoot] = useState("");
  const [streamingText, setStreamingText] = useState("");
  const [streamingToolCalls, setStreamingToolCalls] = useState<Map<string, ToolCallBlock>>(new Map());
  const [pendingPermission, setPendingPermission] = useState<PermissionRequest | null>(null);

  const bottomRef = useRef<HTMLDivElement>(null);
  const streamingTextRef = useRef("");
  const streamingToolCallsRef = useRef<Map<string, ToolCallBlock>>(new Map());

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, streamingText, streamingToolCalls, pendingPermission]);

  // Listen for agent events
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    listenAgentEvent((event) => {
      switch (event.kind) {
        case "TextDelta":
          streamingTextRef.current += event.text;
          setStreamingText(streamingTextRef.current);
          break;
        case "ThinkingDelta":
          break;
        case "ToolUseStart":
          streamingToolCallsRef.current.set(event.id, {
            id: event.id,
            name: event.name,
            input_json: "",
            collapsed: false,
          });
          setStreamingToolCalls(new Map(streamingToolCallsRef.current));
          break;
        case "ToolInputDelta": {
          const tc = streamingToolCallsRef.current.get(event.id);
          if (tc) {
            tc.input_json += event.partial_json;
            setStreamingToolCalls(new Map(streamingToolCallsRef.current));
          }
          break;
        }
        case "ToolResult": {
          const tc = streamingToolCallsRef.current.get(event.id);
          if (tc) {
            tc.result = event.content;
            tc.is_error = event.is_error;
            tc.collapsed = true;
            setStreamingToolCalls(new Map(streamingToolCallsRef.current));
          }
          break;
        }
        case "PermissionRequest":
          setPendingPermission(event.request);
          break;
        case "MessageStop": {
          const finalMessage: ChatMessage = {
            id: `${Date.now()}`,
            role: "Assistant",
            content: streamingTextRef.current,
            tokens: event.output_tokens,
            timestamp: new Date().toISOString(),
            tool_calls: Array.from(streamingToolCallsRef.current.values()),
          };
          setMessages((m) => [...m, finalMessage]);
          setStreamingText("");
          setStreamingToolCalls(new Map());
          streamingTextRef.current = "";
          streamingToolCallsRef.current = new Map();
          setPendingPermission(null);
          setLoading(false);
          onTokenUsage?.({
            input_tokens: event.input_tokens,
            output_tokens: event.output_tokens,
            cached_tokens: event.cached_tokens,
          });
          break;
        }
        case "PhaseChange":
          onPhaseChange?.(event.phase);
          break;
        case "Error":
          setMessages((m) => [
            ...m,
            {
              id: `${Date.now()}`,
              role: "System",
              content: `Error: ${event.message}`,
              timestamp: new Date().toISOString(),
            },
          ]);
          setLoading(false);
          break;
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const handleCreateSession = async () => {
    if (!projectRoot.trim()) return;
    try {
      const s = await sessionCreate(projectRoot, "anthropic", "claude-opus-4-5");
      onSessionCreated(s);
    } catch (e) {
      console.error("Failed to create session", e);
    }
  };

  const appendAssistantMessage = (content: string, tokens?: number) => {
    setMessages((m) => [
      ...m,
      {
        id: `${Date.now()}-${Math.random()}`,
        role: "Assistant",
        content,
        tokens,
        timestamp: new Date().toISOString(),
      },
    ]);
  };

  const handleKanbanSlashCommand = async (userInput: string): Promise<boolean> => {
    if (!session) {
      throw new Error("Create a session before using kanban commands.");
    }
    const trimmed = userInput.trim();
    if (trimmed === "/kanban") {
      const board = await kanbanLoad(session.project_root);
      onOpenKanban?.(board);
      onKanbanUpdated?.(board);
      appendAssistantMessage(`Opened kanban board with ${board.cards.length} cards.`);
      return true;
    }
    if (trimmed.startsWith("/kanban add")) {
      const title = trimmed.replace(/^\/kanban add\s*/, "").trim();
      if (!title) {
        throw new Error("Provide a kanban card title.");
      }
      const board = await kanbanAddCard(session.project_root, title);
      onKanbanUpdated?.(board);
      onOpenKanban?.(board);
      appendAssistantMessage(`Added '${title}' to the backlog.`);
      return true;
    }
    return false;
  };

  const handleSend = async () => {
    if (!input.trim() || !session || loading) return;
    const userInput = input;
    const userMessage: ChatMessage = {
      id: `${Date.now()}`,
      role: "User",
      content: userInput,
      timestamp: new Date().toISOString(),
    };
    setMessages((m) => [...m, userMessage]);
    setInput("");
    setLoading(true);

    try {
      if (await handleKanbanSlashCommand(userInput)) {
        setLoading(false);
        return;
      }
      const response = await agentTurn(session.id, userInput);
      if (userInput.trim().startsWith("/")) {
        appendAssistantMessage(response.content, response.output_tokens);
        onTokenUsage?.({
          input_tokens: response.input_tokens,
          output_tokens: response.output_tokens,
        });
      }
      setLoading(false);
    } catch (e) {
      setMessages((m) => [
        ...m,
        {
          id: `${Date.now()}`,
          role: "System",
          content: `Error: ${e}`,
          timestamp: new Date().toISOString(),
        },
      ]);
      setLoading(false);
    }
  };

  const handlePermission = async (requestId: string, allow: boolean) => {
    await permissionRespond(requestId, allow);
    setPendingPermission(null);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const toggleToolCallCollapse = (messageIdx: number, toolCallId: string) => {
    setMessages((prev) =>
      prev.map((msg, i) => {
        if (i !== messageIdx || !msg.tool_calls) return msg;
        return {
          ...msg,
          tool_calls: msg.tool_calls.map((tc) =>
            tc.id === toolCallId ? { ...tc, collapsed: !tc.collapsed } : tc
          ),
        };
      })
    );
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      {/* Header */}
      <div style={{ padding: "8px 12px", borderBottom: "1px solid #313244", fontSize: 12 }}>
        <strong>AI Chat</strong>
        {session && (
          <span style={{ color: "#6c7086", marginLeft: 8 }}>{session.model_id}</span>
        )}
      </div>

      {/* Session setup (when no session) */}
      {!session && (
        <div style={{ padding: 12 }}>
          <p style={{ color: "#6c7086", marginBottom: 8, fontSize: 12 }}>Open a project to start</p>
          <input
            value={projectRoot}
            onChange={(e) => setProjectRoot(e.target.value)}
            placeholder="/path/to/project"
            style={{
              width: "100%",
              background: "#313244",
              border: "none",
              borderRadius: 4,
              padding: "6px 8px",
              color: "#cdd6f4",
              fontSize: 12,
              marginBottom: 8,
            }}
          />
          <button
            onClick={handleCreateSession}
            style={{
              width: "100%",
              background: "#89b4fa",
              color: "#1e1e2e",
              border: "none",
              borderRadius: 4,
              padding: "6px 0",
              cursor: "pointer",
              fontWeight: 600,
              fontSize: 12,
            }}
          >
            Open Project
          </button>
        </div>
      )}

      {/* Messages */}
      <div style={{ flex: 1, overflowY: "auto", padding: "8px 12px" }}>
        {messages.map((msg, idx) => (
          <MessageBubble
            key={msg.id}
            message={msg}
            onToggleToolCall={(toolCallId) => toggleToolCallCollapse(idx, toolCallId)}
          />
        ))}

        {/* Streaming content */}
        {(streamingText || streamingToolCalls.size > 0) && (
          <div style={{ marginBottom: 12 }}>
            <div style={{ fontSize: 10, color: "#6c7086", marginBottom: 2 }}>Assistant</div>
            <div
              style={{
                background: "#313244",
                borderRadius: 6,
                padding: "6px 10px",
                maxWidth: "95%",
                lineHeight: 1.5,
                fontSize: 12,
              }}
            >
              {streamingText && renderMarkdown(streamingText)}
              {Array.from(streamingToolCalls.values()).map((tc) => (
                <ToolCallView
                  key={tc.id}
                  tc={tc}
                  onToggle={() => {
                    const updated = new Map(streamingToolCallsRef.current);
                    const item = updated.get(tc.id);
                    if (item) {
                      item.collapsed = !item.collapsed;
                      streamingToolCallsRef.current = updated;
                      setStreamingToolCalls(new Map(updated));
                    }
                  }}
                />
              ))}
            </div>
          </div>
        )}

        {/* Permission request */}
        {pendingPermission && (
          <div
            style={{
              margin: "8px 0",
              border: `1px solid ${RISK_COLORS[pendingPermission.risk_level] ?? "#6c7086"}`,
              borderRadius: 6,
              padding: 10,
              background: "#181825",
            }}
          >
            <div
              style={{
                fontWeight: 600,
                color: RISK_COLORS[pendingPermission.risk_level] ?? "#6c7086",
                fontSize: 11,
                marginBottom: 4,
              }}
            >
              ⚠ Permission Request: {pendingPermission.tool_name}
            </div>
            <div style={{ color: "#cdd6f4", fontSize: 11, marginBottom: 8 }}>
              {pendingPermission.description}
            </div>
            <div style={{ display: "flex", gap: 8 }}>
              <button
                onClick={() => handlePermission(pendingPermission.id, true)}
                style={{
                  background: "#a6e3a1",
                  color: "#1e1e2e",
                  border: "none",
                  borderRadius: 4,
                  padding: "4px 12px",
                  cursor: "pointer",
                  fontWeight: 600,
                  fontSize: 11,
                }}
              >
                Approve
              </button>
              <button
                onClick={() => handlePermission(pendingPermission.id, false)}
                style={{
                  background: "#f38ba8",
                  color: "#1e1e2e",
                  border: "none",
                  borderRadius: 4,
                  padding: "4px 12px",
                  cursor: "pointer",
                  fontWeight: 600,
                  fontSize: 11,
                }}
              >
                Deny
              </button>
            </div>
          </div>
        )}

        {loading && !streamingText && streamingToolCalls.size === 0 && (
          <div style={{ color: "#6c7086", fontStyle: "italic", marginTop: 8 }}>Thinking…</div>
        )}
        <div ref={bottomRef} />
      </div>

      {/* Input */}
      {session && (
        <div style={{ padding: 8, borderTop: "1px solid #313244" }}>
          <textarea
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Ask anything… (Enter to send)"
            rows={3}
            disabled={loading}
            style={{
              width: "100%",
              background: "#313244",
              border: "none",
              borderRadius: 4,
              padding: "6px 8px",
              color: "#cdd6f4",
              fontSize: 12,
              resize: "none",
              fontFamily: "inherit",
            }}
          />
        </div>
      )}
    </div>
  );
}

function MessageBubble({
  message,
  onToggleToolCall,
}: {
  message: ChatMessage;
  onToggleToolCall: (toolCallId: string) => void;
}) {
  const isUser = message.role === "User";
  return (
    <div
      style={{
        marginBottom: 12,
        display: "flex",
        flexDirection: "column",
        alignItems: isUser ? "flex-end" : "flex-start",
      }}
    >
      <div style={{ fontSize: 10, color: "#6c7086", marginBottom: 2 }}>
        {message.role}
        {message.tokens ? ` · ${message.tokens} tokens` : ""}
      </div>
      <div
        style={{
          background: isUser ? "#89b4fa22" : "#313244",
          borderRadius: 6,
          padding: "6px 10px",
          maxWidth: "95%",
          lineHeight: 1.5,
          fontSize: 12,
        }}
      >
        {renderMarkdown(message.content)}
        {message.tool_calls?.map((tc) => (
          <ToolCallView key={tc.id} tc={tc} onToggle={() => onToggleToolCall(tc.id)} />
        ))}
      </div>
    </div>
  );
}
