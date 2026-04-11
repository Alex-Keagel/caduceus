import { useEffect, useMemo, useState } from "react";
import {
  marketplaceInstall,
  marketplaceRecommend,
  marketplaceSearch,
  mcpAdd,
  mcpStatus,
} from "../api/tauri";
import type { MarketplaceItem, MarketplaceSearchResult, McpServerInfo } from "../types";
import MarketplaceCard from "./MarketplaceCard";

type MarketplaceTab = "skills" | "agents" | "mcp" | "plugins";

const TAB_LABELS: Record<MarketplaceTab, string> = {
  skills: "Skills",
  agents: "Agents",
  mcp: "MCP Servers",
  plugins: "Plugins",
};

const TAB_ICONS: Record<string, string> = {
  skill: "🧠",
  agent: "🤖",
  plugin: "🧩",
};

const EMPTY_RESULTS: MarketplaceSearchResult = {
  skills: [],
  agents: [],
  plugins: [],
};

function titleCase(value: string) {
  return value
    .split(/[_-\s]+/)
    .filter(Boolean)
    .map((part) => part[0].toUpperCase() + part.slice(1))
    .join(" ");
}

export default function MarketplacePanel() {
  const [activeTab, setActiveTab] = useState<MarketplaceTab>("skills");
  const [query, setQuery] = useState("");
  const [selectedCategory, setSelectedCategory] = useState<string>("All");
  const [searchResults, setSearchResults] = useState<MarketplaceSearchResult>(EMPTY_RESULTS);
  const [recommendedResults, setRecommendedResults] = useState<MarketplaceSearchResult>(EMPTY_RESULTS);
  const [servers, setServers] = useState<McpServerInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [installing, setInstalling] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    const loadRecommendations = async () => {
      try {
        const [recommended, currentServers] = await Promise.all([
          marketplaceRecommend(),
          mcpStatus(),
        ]);

        if (cancelled) return;
        setRecommendedResults(recommended);
        setServers(currentServers);
      } catch (err) {
        if (!cancelled) {
          setError(String(err));
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    };

    void loadRecommendations();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;

    const loadSearchResults = async () => {
      try {
        const results = await marketplaceSearch(query);
        if (!cancelled) {
          setSearchResults(results);
        }
      } catch (err) {
        if (!cancelled) {
          setError(String(err));
        }
      }
    };

    void loadSearchResults();
    return () => {
      cancelled = true;
    };
  }, [query]);

  const currentItems = useMemo(() => {
    const source = query.trim() ? searchResults : recommendedResults;
    switch (activeTab) {
      case "skills":
        return source.skills;
      case "agents":
        return source.agents;
      case "plugins":
        return source.plugins;
      case "mcp":
        return [];
    }
  }, [activeTab, query, recommendedResults, searchResults]);

  const currentCategories = useMemo(() => {
    const categorySet = new Set<string>();
    currentItems.forEach((item) => {
      item.categories.forEach((category) => categorySet.add(category));
    });
    return ["All", ...Array.from(categorySet).sort()];
  }, [currentItems]);

  const filteredItems = useMemo(() => {
    if (selectedCategory === "All") {
      return currentItems;
    }
    return currentItems.filter((item) => item.categories.includes(selectedCategory));
  }, [currentItems, selectedCategory]);

  const filteredServers = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return servers;
    return servers.filter((server) => {
      return (
        server.name.toLowerCase().includes(q) ||
        server.description.toLowerCase().includes(q) ||
        server.status.toLowerCase().includes(q)
      );
    });
  }, [query, servers]);

  useEffect(() => {
    setSelectedCategory("All");
  }, [activeTab]);

  const handleInstall = async (item: MarketplaceItem) => {
    setInstalling(item.name);
    setError(null);

    try {
      await marketplaceInstall(item.name);
      const markInstalled = (results: MarketplaceSearchResult): MarketplaceSearchResult => ({
        skills: results.skills.map((entry) =>
          entry.name === item.name ? { ...entry, installed: true } : entry
        ),
        agents: results.agents.map((entry) =>
          entry.name === item.name ? { ...entry, installed: true } : entry
        ),
        plugins: results.plugins.map((entry) =>
          entry.name === item.name ? { ...entry, installed: true } : entry
        ),
      });

      setSearchResults((prev) => markInstalled(prev));
      setRecommendedResults((prev) => markInstalled(prev));
    } catch (err) {
      setError(String(err));
    } finally {
      setInstalling(null);
    }
  };

  const handleAddServer = async (name: string) => {
    setInstalling(name);
    setError(null);

    try {
      await mcpAdd(name);
      setServers((prev) =>
        prev.map((server) =>
          server.name === name ? { ...server, connected: true, status: "connected" } : server
        )
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setInstalling(null);
    }
  };

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 16,
        height: "100%",
        padding: 16,
        overflow: "hidden",
        background: "#11111b",
      }}
    >
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12 }}>
        <div>
          <div style={{ fontSize: 18, fontWeight: 700 }}>Marketplace</div>
          <div style={{ fontSize: 12, color: "#6c7086", marginTop: 4 }}>
            Discover skills, agents, MCP servers, and plugins for your workflow.
          </div>
        </div>
      </div>

      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        {(Object.keys(TAB_LABELS) as MarketplaceTab[]).map((tab) => (
          <button
            key={tab}
            type="button"
            onClick={() => setActiveTab(tab)}
            style={{
              border: "1px solid #313244",
              borderRadius: 999,
              padding: "8px 12px",
              background: activeTab === tab ? "#89b4fa" : "#181825",
              color: activeTab === tab ? "#1e1e2e" : "#cdd6f4",
              fontWeight: 700,
              cursor: "pointer",
            }}
          >
            {TAB_LABELS[tab]}
          </button>
        ))}
      </div>

      <input
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        placeholder={`Search ${TAB_LABELS[activeTab].toLowerCase()}…`}
        style={{
          width: "100%",
          background: "#181825",
          border: "1px solid #313244",
          borderRadius: 10,
          padding: "12px 14px",
          color: "#cdd6f4",
          fontSize: 13,
          outline: "none",
        }}
      />

      {activeTab !== "mcp" && (
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          {currentCategories.map((category) => (
            <button
              key={category}
              type="button"
              onClick={() => setSelectedCategory(category)}
              style={{
                border: "none",
                borderRadius: 999,
                padding: "6px 10px",
                background: selectedCategory === category ? "#cba6f7" : "#313244",
                color: selectedCategory === category ? "#1e1e2e" : "#cdd6f4",
                cursor: "pointer",
                fontSize: 11,
                fontWeight: 700,
              }}
            >
              {titleCase(category)}
            </button>
          ))}
        </div>
      )}

      {error && (
        <div
          style={{
            border: "1px solid #f38ba8",
            background: "#f38ba81a",
            color: "#f5c2e7",
            borderRadius: 10,
            padding: "10px 12px",
            fontSize: 12,
          }}
        >
          {error}
        </div>
      )}

      <div style={{ flex: 1, overflowY: "auto" }}>
        {loading ? (
          <div style={{ color: "#6c7086", fontSize: 12 }}>Loading marketplace…</div>
        ) : activeTab === "mcp" ? (
          <div
            style={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fit, minmax(260px, 1fr))",
              gap: 12,
            }}
          >
            {filteredServers.map((server) => (
              <MarketplaceCard
                key={server.name}
                icon="🔌"
                name={server.name}
                description={server.description}
                categories={[titleCase(server.source), `Status: ${titleCase(server.status)}`]}
                installed={server.connected}
                actionLabel="Add Server"
                disabled={installing === server.name}
                onInstall={() => handleAddServer(server.name)}
                extraContent={
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 8,
                      fontSize: 12,
                      color: "#bac2de",
                    }}
                  >
                    <span
                      style={{
                        width: 10,
                        height: 10,
                        borderRadius: "50%",
                        background: server.connected ? "#a6e3a1" : "#f38ba8",
                        display: "inline-block",
                      }}
                    />
                    {server.connected ? "Connected" : "Disconnected"}
                  </div>
                }
              />
            ))}
          </div>
        ) : (
          <div
            style={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fit, minmax(260px, 1fr))",
              gap: 12,
            }}
          >
            {filteredItems.map((item) => (
              <MarketplaceCard
                key={`${item.kind}-${item.name}`}
                icon={TAB_ICONS[item.kind] ?? "⬢"}
                name={item.name}
                description={item.description}
                categories={item.categories.map(titleCase)}
                installed={item.installed}
                disabled={installing === item.name}
                onInstall={() => handleInstall(item)}
              />
            ))}
          </div>
        )}

        {!loading && activeTab !== "mcp" && filteredItems.length === 0 && (
          <div style={{ color: "#6c7086", fontSize: 12 }}>No marketplace entries match this filter.</div>
        )}

        {!loading && activeTab === "mcp" && filteredServers.length === 0 && (
          <div style={{ color: "#6c7086", fontSize: 12 }}>No MCP servers match this search.</div>
        )}
      </div>
    </div>
  );
}
