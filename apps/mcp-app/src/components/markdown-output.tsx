import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import rehypeRaw from "rehype-raw";

interface MarkdownOutputProps {
  content: string;
}

export function MarkdownOutput({ content }: MarkdownOutputProps) {
  if (!content) return null;
  return (
    <div className="markdown-output">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[rehypeKatex, rehypeRaw]}
        components={{
          code({ className, children }) {
            const codeContent = String(children).replace(/\n$/, "");
            const isBlock = codeContent.includes("\n") || className;
            if (isBlock) {
              return (
                <pre className="code-block">
                  <code>{codeContent}</code>
                </pre>
              );
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
