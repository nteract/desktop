import { SyntaxHighlighter, oneDark, oneLight } from "../lib/syntax-highlighter";

interface CodeBlockProps {
  code: string;
  language?: string;
  isDark?: boolean;
}

/**
 * Syntax-highlighted code block using PrismLight.
 * Falls back to plain text for unregistered languages.
 */
export function CodeBlock({ code, language = "", isDark }: CodeBlockProps) {
  const dark = isDark ?? (typeof window !== "undefined" && window.matchMedia("(prefers-color-scheme: dark)").matches);

  return (
    <SyntaxHighlighter
      language={language}
      style={dark ? oneDark : oneLight}
      PreTag="div"
      customStyle={{
        margin: 0,
        padding: "10px 12px",
        fontSize: "13px",
        overflow: "auto",
        background: dark ? "#282c34" : "#fafafa",
        borderRadius: "6px",
        border: `1px solid ${dark ? "#374151" : "#e5e7eb"}`,
        lineHeight: "1.5",
      }}
    >
      {code}
    </SyntaxHighlighter>
  );
}
