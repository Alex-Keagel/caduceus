import hljs from "highlight.js/lib/common";
import { useMemo } from "react";

interface SyntaxHighlighterProps {
  code: string;
  language?: string;
}

export const SUPPORTED_LANGUAGES = [
  "bash",
  "c",
  "cpp",
  "css",
  "go",
  "java",
  "javascript",
  "json",
  "python",
  "rust",
  "sql",
  "swift",
  "tsx",
  "typescript",
  "xml",
  "yaml",
];

function normalizeLanguage(language?: string): string | undefined {
  const value = language?.trim().toLowerCase();
  if (!value) return undefined;
  if (value === "ts") return "typescript";
  if (value === "js") return "javascript";
  if (value === "sh" || value === "shell" || value === "zsh") return "bash";
  if (value === "html") return "xml";
  return value;
}

export default function SyntaxHighlighter({ code, language }: SyntaxHighlighterProps) {
  const normalized = normalizeLanguage(language);
  const html = useMemo(() => {
    try {
      if (normalized && hljs.getLanguage(normalized)) {
        return hljs.highlight(code, { language: normalized }).value;
      }
      return hljs.highlightAuto(code).value;
    } catch {
      return hljs.highlightAuto(code).value;
    }
  }, [code, normalized]);

  return (
    <pre className="syntax-highlighter">
      <div className="syntax-highlighter__header">
        <span>{normalized ?? "auto"}</span>
        <span>{SUPPORTED_LANGUAGES.length}+ langs</span>
      </div>
      <code dangerouslySetInnerHTML={{ __html: html }} />
    </pre>
  );
}
