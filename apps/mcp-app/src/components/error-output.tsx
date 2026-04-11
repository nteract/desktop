import { useEffect, useState } from "react";
import { AnsiText } from "./ansi-text";
import { isBlobUrl } from "../lib/blob-fetch";
import type { CellOutput } from "../types";

interface ErrorOutputProps {
  output: CellOutput;
}

export function ErrorOutput({ output }: ErrorOutputProps) {
  const [tracebackLines, setTracebackLines] = useState<string[]>([]);
  const [fetchFailed, setFetchFailed] = useState(false);

  const header = output.ename ? `${output.ename}: ${output.evalue || ""}` : "";

  useEffect(() => {
    const tb = output.traceback;
    if (Array.isArray(tb)) {
      setTracebackLines(tb);
    } else if (typeof tb === "string" && isBlobUrl(tb)) {
      fetch(tb)
        .then((r) => r.json())
        .then((lines: string[]) => {
          if (Array.isArray(lines)) setTracebackLines(lines);
        })
        .catch(() => setFetchFailed(true));
    }
  }, [output.traceback]);

  return (
    <div className="error-output">
      {header && <AnsiText text={header} />}
      {tracebackLines.length > 0 && <AnsiText text={tracebackLines.join("\n")} />}
      {fetchFailed && tracebackLines.length === 0 && (
        <div style={{ opacity: 0.6, fontSize: "12px", marginTop: "4px" }}>
          (traceback could not be loaded)
        </div>
      )}
    </div>
  );
}
