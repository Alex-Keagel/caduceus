export interface AgentWindowTab {
  id: string;
  title: string;
  status: "idle" | "running" | "complete" | "error";
  subtitle?: string | null;
}

interface AgentsWindowProps {
  tabs: AgentWindowTab[];
  activeTabId: string;
  onSelect: (tabId: string) => void;
  onAdd: () => void;
  onClose: (tabId: string) => void;
  onRename: (tabId: string) => void;
}

const STATUS_CLASS: Record<AgentWindowTab["status"], string> = {
  idle: "agent-window__status--idle",
  running: "agent-window__status--running",
  complete: "agent-window__status--complete",
  error: "agent-window__status--error",
};

export default function AgentsWindow({
  tabs,
  activeTabId,
  onSelect,
  onAdd,
  onClose,
  onRename,
}: AgentsWindowProps) {
  return (
    <div className="agent-window">
      <div className="agent-window__tabs">
        {tabs.map((tab) => {
          const active = tab.id === activeTabId;
          return (
            <button
              key={tab.id}
              type="button"
              className={`agent-window__tab ${active ? "agent-window__tab--active" : ""}`}
              onClick={() => onSelect(tab.id)}
              onDoubleClick={() => onRename(tab.id)}
            >
              <span className={`agent-window__status ${STATUS_CLASS[tab.status]}`} />
              <span className="agent-window__title-group">
                <span>{tab.title}</span>
                {tab.subtitle ? <small>{tab.subtitle}</small> : null}
              </span>
              {tabs.length > 1 ? (
                <span
                  className="agent-window__close"
                  onClick={(event) => {
                    event.stopPropagation();
                    onClose(tab.id);
                  }}
                >
                  ✕
                </span>
              ) : null}
            </button>
          );
        })}
        <button type="button" className="agent-window__add" onClick={onAdd}>
          + Add agent
        </button>
      </div>
    </div>
  );
}
