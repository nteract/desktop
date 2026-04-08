import { SyntaxHighlighter, oneDark, oneLight } from "../lib/syntax-highlighter";

interface CodeBlockProps {
  code: string;
  language?: string;
}

/**
 * Detect dark mode from data-theme attribute (set by host via ext-apps SDK)
 * with prefers-color-scheme fallback.
 */
function isDarkMode(): boolean {
  if (typeof document === "undefined") return false;
  const theme = document.documentElement.getAttribute("data-theme");
  if (theme === "dark") return true;
  if (theme === "light") return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/**
 * Syntax-highlighted code block using PrismLight.
 * Respects the host theme via data-theme attribute.
 */
export function CodeBlock({ code, language = "" }: CodeBlockProps) {
  const dark = isDarkMode();

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
        borderRadius: "6px",
        lineHeight: "1.5",
      }}
    >
      {code}
    </SyntaxHighlighter>
  );
}
