import type { CellData } from "../types";
import { MimeRenderer, StreamOutput } from "./mime-renderer";
import { ErrorOutput } from "./error-output";

interface CellOutputProps {
  cell: CellData;
}

export function CellOutput({ cell }: CellOutputProps) {
  if (!cell.outputs?.length) {
    return null;
  }

  return (
    <div className="cell">
      {cell.source && (
        <details className="source-details">
          <summary className="source-summary">Source</summary>
          <pre className="source">{cell.source}</pre>
        </details>
      )}
      <div className="outputs">
        {cell.outputs.map((output, i) => {
          switch (output.output_type) {
            case "stream":
              return <StreamOutput key={i} output={output} />;
            case "error":
              return <ErrorOutput key={i} output={output} />;
            case "display_data":
            case "execute_result":
              if (output.data) {
                return <MimeRenderer key={i} data={output.data} />;
              }
              return null;
            default:
              return null;
          }
        })}
      </div>
    </div>
  );
}
