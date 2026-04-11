import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from "react";
import {
  agentAbort,
  agentTurn,
  listenAgentEvent,
  permissionRespond,
  sessionCreate,
  sessionMessages,
} from "../api/tauri";
import type {
  ChatMessage,
  PermissionRequest,
  SessionInfo,
  SessionPhase,
  TokenUsage,
  ToolCallBlock,
  TranscriptEntry,
} from "../types";
import type { UnlistenFn } from "@tauri-apps/api/event";
import ModelPicker from "./ModelPicker";
import SyntaxHighlighter from "./SyntaxHighlighter";
import { emitDesktopNotification } from "./DesktopNotifications";

interface ChatProps {
  session: SessionInfo | null;
  onSessionCreated: (session: SessionInfo) => void;
  onSessionUpdated: (session: SessionInfo) => void;
  onPhaseChange?: (phase: SessionPhase) => void;
  onTokenUsage?: (usage: TokenUsage) => void;
}

export interface ChatHandle {
  focusInput: () => void;
  sendMessage: () => void;
  sendRaw: (value: string) => void;
  cancelAgent: () => Promise<void>;
}

function transcriptToMessage(entry: TranscriptEntry): ChatMessage {
  return {
    id: `${entry.timestamp}-${entry.role}-${Math.random().toString(16).slice(2)}`,
    role: entry.role,
    content: entry.content,
    tokens: entry.tokens,
    timestamp: entry.timestamp,
  };
}

function renderMarkdown(text: string): React.ReactNode {
  if (!text) return null;
  const parts = text.split(/(```[\w-]*\n[\s\S]*?```|`[^`\n]+`)/g);
  return (
    <>
      {parts.map((part, index) => {
        if (part.startsWith("```")) {
          const header = part.slice(3, part.indexOf("\n") >= 0 ? part.indexOf("\n") : undefined);
          const code = part.replace(/^```[\w-]*\n?/, "").replace(/```$/, "");
          return <SyntaxHighlighter key={index} code={code} language={header} />;
        }
        if (part.startsWith("`") && part.endsWith("`")) {
          return (
            <code key={index} className="chat-inline-code">
              {part.slice(1, -1)}
            </code>
          );
        }
        return (
          <span key={index} style={{ whiteSpace: "pre-wrap" }}>
            {part}
          </span>
        );
      })}
    </>
  );
}

function ToolCallView({ tc, onToggle }: { tc: ToolCallBlock; onToggle: () => void }) {
  const resultColor = tc.is_error ? "var(--color-danger)" : "var(--color-success)";
  return (
    <div className="tool-call-card">
      <div className="tool-call-card__header" onClick={onToggle}>
        <span style={{ color: "var(--color-accent)" }}>⚙ {tc.name}</span>
        {tc.result !== undefined ? <span style={{ color: resultColor }}>{tc.is_error ? "✗" : "✓"}</span> : null}
        <span style={{ color: "var(--color-muted)" }}>{tc.collapsed ? "▶" : "▼"}</span>
      </div>
      {!tc.collapsed ? (
        <div className="tool-call-card__body">
          {tc.input_json ? <pre>{tc.input_json}</pre> : null}
          {tc.result !== undefined ? <pre style={{ color: resultColor }}>{tc.result}</pre> : null}
        </div>
      ) : null}
    </div>
  );
}

const Chat = forwardRef<ChatHandle, ChatProps>(function Chat(
  { session, onSessionCreated, onSessionUpdated, onPhaseChange, onTokenUsage }: ChatProps,
  ref
) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [loading, setLoading] = useState(false);
  const [projectRoot, setProjectRoot] = useState(session?.project_root ?? "");
  const [newSessionModel, setNewSessionModel] = useState("claude-opus-4-5");
  const [streamingText, setStreamingText] = useState("");
  const [streamingToolCalls, setStreamingToolCalls] = useState<Map<string, ToolCallBlock>>(new Map());
  const [pendingPermission, setPendingPermission] = useState<PermissionRequest | null>(null);

  const bottomRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const streamingTextRef = useRef("");
  const streamingToolCallsRef = useRef<Map<string, ToolCallBlock>>(new Map());

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, streamingText, streamingToolCalls, pendingPermission]);

  useEffect(() => {
    setProjectRoot(session?.project_root ?? "");
  }, [session?.project_root]);

  useEffect(() => {
    let cancelled = false;
    const loadHistory = async () => {
      if (!session) {
        setMessages([]);
        return;
      }
      const history = await sessionMessages(session.id);
      if (!cancelled) {
        setMessages(history.map(transcriptToMessage));
        setStreamingText("");
        streamingTextRef.current = "";
        setStreamingToolCalls(new Map());
        streamingToolCallsRef.current = new Map();
        setPendingPermission(null);
      }
    };
    void loadHistory();
    return () => {
      cancelled = true;
    };
  }, [session?.id]);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    void listenAgentEvent((event) => {
      switch (event.type) {
        case "TextDelta":
          streamingTextRef.current += event.text;
          setStreamingText(streamingTextRef.current);
          break;
        case "ToolCallStart":
          streamingToolCallsRef.current.set(event.id, {
            id: event.id,
            name: event.name,
            input_json: "",
            collapsed: false,
          });
          setStreamingToolCalls(new Map(streamingToolCallsRef.current));
          break;
        case "ToolCallInput": {
          const item = streamingToolCallsRef.current.get(event.id);
          if (item) {
            item.input_json += event.delta;
            setStreamingToolCalls(new Map(streamingToolCallsRef.current));
          }
          break;
        }
        case "ToolResultEnd": {
          const item = streamingToolCallsRef.current.get(event.id);
          if (item) {
            item.result = event.content;
            item.is_error = event.is_error;
            item.collapsed = true;
            setStreamingToolCalls(new Map(streamingToolCallsRef.current));
          }
          break;
        }
        case "PermissionRequest":
          setPendingPermission({
            id: event.id,
            capability: event.capability,
            description: event.description,
          });
          emitDesktopNotification({
            title: "Caduceus needs permission",
            body: event.description,
          });
          break;
        case "TurnComplete": {
          const finalMessage: ChatMessage = {
            id: `${Date.now()}`,
            role: "Assistant",
            content: streamingTextRef.current,
            tokens: event.usage.output_tokens,
            timestamp: new Date().toISOString(),
            tool_calls: Array.from(streamingToolCallsRef.current.values()),
          };
          if (finalMessage.content || finalMessage.tool_calls?.length) {
            setMessages((existing) => [...existing, finalMessage]);
          }
          setStreamingText("");
          streamingTextRef.current = "";
          setStreamingToolCalls(new Map());
          streamingToolCallsRef.current = new Map();
          setPendingPermission(null);
          setLoading(false);
          onTokenUsage?.(event.usage);
          emitDesktopNotification({
            title: "Agent completed",
            body: session ? `${session.project_root.split("/").pop()} finished a turn.` : "Agent completed its turn.",
          });
          break;
        }
        case "SessionPhaseChanged":
          onPhaseChange?.(event.phase);
          break;
        case "Error":
          setMessages((existing) => [
            ...existing,
            {
              id: `${Date.now()}`,
              role: "System",
              content: `Error: ${event.message}`,
              timestamp: new Date().toISOString(),
            },
          ]);
          setLoading(false);
          emitDesktopNotification({
            title: "Agent error",
            body: event.message,
          });
          break;
        case "ToolCallEnd":
        case "ToolResultStart":
          break;
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [onPhaseChange, onTokenUsage, session]);

  const handleCreateSession = async () => {
    if (!projectRoot.trim()) return;
    try {
      const created = await sessionCreate(projectRoot, "anthropic", newSessionModel);
      onSessionCreated(created);
    } catch (error) {
      console.error("Failed to create session", error);
    }
  };

  const appendSystemMessage = (content: string) => {
    setMessages((existing) => [
      ...existing,
      {
        id: `${Date.now()}-${Math.random().toString(16).slice(2)}`,
        role: "System",
        content,
        timestamp: new Date().toISOString(),
      },
    ]);
  };

  const sendInput = async (messageValue: string, clearInput = false) => {
    if (!messageValue.trim() || !session || loading) return;
    const userInput = messageValue;
    setMessages((existing) => [
      ...existing,
      {
        id: `${Date.now()}`,
        role: "User",
        content: userInput,
        timestamp: new Date().toISOString(),
      },
    ]);
    if (clearInput) {
      setInput("");
    }
    setLoading(true);

    try {
      const response = await agentTurn(session.id, userInput);
      if (response.warning) {
        appendSystemMessage(response.warning);
      }
      if (response.session) {
        onSessionUpdated(response.session);
      }
      if (userInput.trim().startsWith("/")) {
        setMessages((existing) => [
          ...existing,
          {
            id: `${Date.now()}-${Math.random().toString(16).slice(2)}`,
            role: "Assistant",
            content: response.content,
            tokens: response.output_tokens,
            timestamp: new Date().toISOString(),
          },
        ]);
        onTokenUsage?.({
          input_tokens: response.input_tokens,
          output_tokens: response.output_tokens,
        });
        setLoading(false);
      }
    } catch (error) {
      appendSystemMessage(`Error: ${String(error)}`);
      setLoading(false);
    }
  };

  const handleSend = async () => {
    await sendInput(input, true);
  };

  useImperativeHandle(
    ref,
    () => ({
      focusInput: () => inputRef.current?.focus(),
      sendMessage: () => {
        void handleSend();
      },
      sendRaw: (value: string) => {
        void sendInput(value, false);
      },
      cancelAgent: async () => {
        if (!session) return;
        await agentAbort(session.id);
      },
    }),
    [handleSend, session, input]
  );

  const handlePermission = async (requestId: string, allow: boolean) => {
    await permissionRespond(requestId, allow);
    setPendingPermission(null);
  };

  const handleKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "Enter" && !event.shiftKey && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      void handleSend();
    }
  };

  const toggleToolCallCollapse = (messageIdx: number, toolCallId: string) => {
    setMessages((existing) =>
      existing.map((message, index) => {
        if (index !== messageIdx || !message.tool_calls) return message;
        return {
          ...message,
          tool_calls: message.tool_calls.map((toolCall) =>
            toolCall.id === toolCallId ? { ...toolCall, collapsed: !toolCall.collapsed } : toolCall
          ),
        };
      })
    );
  };

  return (
    <div className="chat-shell">
      <div className="chat-header">
        <div>
          <strong>Agent Chat</strong>
          {session ? <span className="chat-header__meta">{session.project_root}</span> : null}
        </div>
        {session ? (
          <ModelPicker
            value={session.model_id}
            onChange={(modelId) => {
              void sendInput(`/model ${modelId}`);
            }}
          />
        ) : null}
      </div>

      {!session ? (
        <div className="chat-empty-state">
          <p>Open a project to start an agent session.</p>
          <input
            value={projectRoot}
            onChange={(event) => setProjectRoot(event.target.value)}
            placeholder="/path/to/project"
          />
          <ModelPicker value={newSessionModel} onChange={setNewSessionModel} />
          <button type="button" onClick={handleCreateSession}>
            Open project
          </button>
        </div>
      ) : null}

      <div className="chat-messages">
        {messages.map((message, index) => (
          <MessageBubble
            key={message.id}
            message={message}
            onToggleToolCall={(toolCallId) => toggleToolCallCollapse(index, toolCallId)}
          />
        ))}

        {(streamingText || streamingToolCalls.size > 0) && (
          <div className="message-row">
            <div className="message-role">Assistant</div>
            <div className="message-bubble">
              {streamingText ? renderMarkdown(streamingText) : null}
              {Array.from(streamingToolCalls.values()).map((toolCall) => (
                <ToolCallView
                  key={toolCall.id}
                  tc={toolCall}
                  onToggle={() => {
                    const updated = new Map(streamingToolCallsRef.current);
                    const item = updated.get(toolCall.id);
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

        {pendingPermission ? (
          <div className="permission-card">
            <div className="permission-card__title">⚠ Permission request · {pendingPermission.capability}</div>
            <div className="permission-card__body">{pendingPermission.description}</div>
            <div className="permission-card__actions">
              <button type="button" onClick={() => void handlePermission(pendingPermission.id, true)}>
                Approve
              </button>
              <button type="button" className="danger" onClick={() => void handlePermission(pendingPermission.id, false)}>
                Deny
              </button>
            </div>
          </div>
        ) : null}

        {loading && !streamingText && streamingToolCalls.size === 0 ? (
          <div className="chat-loading">Thinking…</div>
        ) : null}
        <div ref={bottomRef} />
      </div>

      {session ? (
        <div className="chat-input-shell">
          <textarea
            ref={inputRef}
            value={input}
            onChange={(event) => setInput(event.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Ask anything… (Enter to send)"
            rows={3}
            disabled={loading}
          />
        </div>
      ) : null}
    </div>
  );
});

function MessageBubble({
  message,
  onToggleToolCall,
}: {
  message: ChatMessage;
  onToggleToolCall: (toolCallId: string) => void;
}) {
  const isUser = message.role === "User";
  return (
    <div className={`message-row ${isUser ? "message-row--user" : ""}`}>
      <div className="message-role">
        {message.role}
        {message.tokens ? ` · ${message.tokens} tokens` : ""}
      </div>
      <div className={`message-bubble ${isUser ? "message-bubble--user" : ""}`}>
        {renderMarkdown(message.content)}
        {message.tool_calls?.map((toolCall) => (
          <ToolCallView key={toolCall.id} tc={toolCall} onToggle={() => onToggleToolCall(toolCall.id)} />
        ))}
      </div>
    </div>
  );
}

export default Chat;
