import ReactMarkdown from "react-markdown";
import rehypeKatex from "rehype-katex";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import { CodeBlock } from "./code-block";

interface MarkdownOutputProps {
  content: string;
}

export function MarkdownOutput({ content }: MarkdownOutputProps) {
  if (!content) return null;
  return (
    <div className="markdown-output">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[rehypeKatex]}
        components={{
          code({ className, children }) {
            const codeContent = String(children).replace(/\n$/, "");
            const match = /language-(\w+)/.exec(className || "");
            const language = match ? match[1] : "";
            const isBlock = codeContent.includes("\n") || className;
            if (isBlock) {
              return <CodeBlock code={codeContent} language={language} />;
            }
            return <code className="inline-code">{children}</code>;
          },
          a({ href, children, ...props }) {
            return (
              <a href={href} rel="noopener noreferrer" target="_blank" {...props}>
                {children}
              </a>
            );
          },
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
