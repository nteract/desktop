import Anser from "anser";
import { escapeCarriageReturn } from "escape-carriage";

interface AnsiTextProps {
  text: string;
  className?: string;
}

export function AnsiText({ text, className = "" }: AnsiTextProps) {
  if (!text) return null;

  const escaped = escapeCarriageReturn(text);
  const spans = Anser.ansiToJson(escaped, { use_classes: true });

  return (
    <pre className={`ansi-output ${className}`}>
      {spans.map((span, i) => {
        const style: React.CSSProperties = {};
        if (span.fg_truecolor) {
          style.color = `rgb(${span.fg_truecolor})`;
        } else if (span.fg) {
          style.color = `var(--${span.fg})`;
        }
        if (span.bg_truecolor) {
          style.backgroundColor = `rgb(${span.bg_truecolor})`;
        } else if (span.bg) {
          style.backgroundColor = `var(--${span.bg})`;
        }
        if (span.decoration === "bold") style.fontWeight = "bold";
        if (span.decoration === "italic") style.fontStyle = "italic";
        if (span.decoration === "underline") style.textDecoration = "underline";

        return (
          <span key={i} style={style}>
            {span.content}
          </span>
        );
      })}
    </pre>
  );
}
