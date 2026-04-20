import { CellContainer } from "@/components/cell/CellContainer";
import { ExecutionCount } from "@/components/cell/ExecutionCount";
import { ExecutionStatus } from "@/components/cell/ExecutionStatus";
import { AnsiErrorOutput, AnsiStreamOutput } from "@/components/outputs/ansi-output";
import { MediaRouter } from "@/components/outputs/media-router";
import { CodeMirrorEditor } from "@/components/editor/codemirror-editor";
import type { JupyterOutput } from "@/components/cell/jupyter-output";
import type { ExecutionState } from "runtimed/src/runtime-state";

interface CellData {
  id: string;
  cell_type: string;
  source: string;
  execution_count: number | null;
  outputs: JupyterOutput[];
}

interface Props {
  cell: CellData;
  executionState: ExecutionState | null;
}

export function CellView({ cell, executionState }: Props) {
  const isRunning = executionState?.status === "running";
  const isQueued = executionState?.status === "queued";

  return (
    <CellContainer
      id={cell.id}
      cellType={cell.cell_type}
      gutterContent={
        cell.cell_type === "code" ? (
          <div className="flex flex-col items-end gap-0.5">
            <ExecutionCount count={cell.execution_count} isExecuting={isRunning} />
            {(isRunning || isQueued) && <ExecutionStatus executionState={executionState.status} />}
          </div>
        ) : undefined
      }
      codeContent={
        cell.cell_type === "markdown" ? (
          <pre className="whitespace-pre-wrap break-words text-[13px] text-muted-foreground">
            {cell.source}
          </pre>
        ) : (
          <CodeMirrorEditor
            initialValue={cell.source}
            language={cell.cell_type === "code" ? "python" : undefined}
            readOnly
          />
        )
      }
      outputContent={
        cell.outputs.length > 0 ? (
          <div className="space-y-2 pl-6 pr-3">
            {cell.outputs.map((output, i) => (
              <CellOutput key={i} output={output} />
            ))}
          </div>
        ) : undefined
      }
    />
  );
}

function CellOutput({ output }: { output: JupyterOutput }) {
  if (output.output_type === "stream") {
    const text = Array.isArray(output.text) ? output.text.join("") : output.text;
    return <AnsiStreamOutput text={text} streamName={output.name} />;
  }

  if (output.output_type === "error") {
    return (
      <AnsiErrorOutput ename={output.ename} evalue={output.evalue} traceback={output.traceback} />
    );
  }

  if (output.output_type === "execute_result" || output.output_type === "display_data") {
    return (
      <MediaRouter
        data={output.data}
        metadata={output.metadata as Record<string, Record<string, unknown> | undefined>}
      />
    );
  }

  return null;
}
