import { useState, useRef, useEffect } from "react";
import { agentTurn, sessionCreate } from "../api/tauri";
import type { SessionInfo, TranscriptEntry } from "../types";

interface ChatProps {
  session: SessionInfo | null;
  onSessionCreated: (session: SessionInfo) => void;
}

export default function Chat({ session, onSessionCreated }: ChatProps) {
  const [messages, setMessages] = useState<TranscriptEntry[]>([]);
  const [input, setInput] = useState("");
  const [loading, setLoading] = useState(false);
  const [projectRoot, setProjectRoot] = useState("");
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  const handleCreateSession = async () => {
    if (!projectRoot.trim()) return;
    try {
      const s = await sessionCreate(projectRoot, "anthropic", "claude-opus-4-5");
      onSessionCreated(s);
    } catch (e) {
      console.error("Failed to create session", e);
    }
  };

  const handleSend = async () => {
    if (!input.trim() || !session || loading) return;
    const userMessage: TranscriptEntry = {
      role: "User",
      content: input,
      timestamp: new Date().toISOString(),
    };
    setMessages((m) => [...m, userMessage]);
    setInput("");
    setLoading(true);

    try {
      const response = await agentTurn(session.id, userMessage.content);
      const assistantMessage: TranscriptEntry = {
        role: "Assistant",
        content: response.content,
        tokens: response.output_tokens,
        timestamp: new Date().toISOString(),
      };
      setMessages((m) => [...m, assistantMessage]);
    } catch (e) {
      const errorMessage: TranscriptEntry = {
        role: "System",
        content: `Error: ${e}`,
        timestamp: new Date().toISOString(),
      };
      setMessages((m) => [...m, errorMessage]);
    } finally {
      setLoading(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      {/* Header */}
      <div style={{ padding: "8px 12px", borderBottom: "1px solid #313244", fontSize: 12 }}>
        <strong>AI Chat</strong>
        {session && (
          <span style={{ color: "#6c7086", marginLeft: 8 }}>
            {session.model_id}
          </span>
        )}
      </div>

      {/* Session setup (when no session) */}
      {!session && (
        <div style={{ padding: 12 }}>
          <p style={{ color: "#6c7086", marginBottom: 8, fontSize: 12 }}>
            Open a project to start
          </p>
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
        {messages.map((msg, i) => (
          <MessageBubble key={i} entry={msg} />
        ))}
        {loading && (
          <div style={{ color: "#6c7086", fontStyle: "italic", marginTop: 8 }}>
            Thinking…
          </div>
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

function MessageBubble({ entry }: { entry: TranscriptEntry }) {
  const isUser = entry.role === "User";
  return (
    <div
      style={{
        marginBottom: 12,
        display: "flex",
        flexDirection: "column",
        alignItems: isUser ? "flex-end" : "flex-start",
      }}
    >
      <div
        style={{
          fontSize: 10,
          color: "#6c7086",
          marginBottom: 2,
        }}
      >
        {entry.role}
        {entry.tokens ? ` · ${entry.tokens} tokens` : ""}
      </div>
      <div
        style={{
          background: isUser ? "#89b4fa22" : "#313244",
          borderRadius: 6,
          padding: "6px 10px",
          maxWidth: "95%",
          whiteSpace: "pre-wrap",
          lineHeight: 1.5,
          fontSize: 12,
        }}
      >
        {entry.content}
      </div>
    </div>
  );
}
