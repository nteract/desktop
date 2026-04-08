interface CodeBlockProps {
  code: string;
  language?: string;
}

export function CodeBlock({ code, language = "" }: CodeBlockProps) {
  return (
    <pre className="code-block">
      <code className={language ? `language-${language}` : ""}>{code}</code>
    </pre>
  );
}
