import { getGutterColors } from "@/components/cell/gutter-colors";
import { AnsiOutput } from "@/components/outputs/ansi-output";

interface OutputData {
  output_type: string;
  text?: string;
  data?: Record<string, unknown>;
  name?: string;
  ename?: string;
  evalue?: string;
  traceback?: string[];
}

interface CellData {
  id: string;
  cell_type: string;
  source: string;
  execution_count: number | null;
  outputs: OutputData[];
}

interface Props {
  cell: CellData;
}

export function CellView({ cell }: Props) {
  const colors = getGutterColors(cell.cell_type);

  return (
    <div className="group flex items-stretch py-0.5">
      {/* Ribbon */}
      <div className={`w-[3px] shrink-0 rounded-sm ${colors.ribbon.default} my-0.5 ml-2`} />

      {/* Gutter */}
      <div className="flex w-10 shrink-0 items-start justify-end px-1.5 pt-1.5 font-mono text-[10px] text-muted-foreground select-none">
        {cell.cell_type === "code" && cell.execution_count != null && (
          <span>[{cell.execution_count}]</span>
        )}
      </div>

      {/* Body */}
      <div className="min-w-0 flex-1 py-0.5">
        {/* Source */}
        <div
          className={`rounded-md border border-border px-3 py-2 font-mono text-[13px] leading-relaxed ${
            cell.cell_type === "markdown"
              ? "border-transparent bg-transparent text-muted-foreground"
              : "bg-background"
          }`}
        >
          <pre className="whitespace-pre-wrap break-words">{cell.source}</pre>
        </div>

        {/* Outputs */}
        {cell.outputs.length > 0 && (
          <div className="mt-1 space-y-1 pl-0.5">
            {cell.outputs.map((output, i) => (
              <CellOutput key={i} output={output} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function CellOutput({ output }: { output: OutputData }) {
  if (output.output_type === "stream") {
    const text = output.text ?? "";
    const isStderr = output.name === "stderr";
    return (
      <div className={`font-mono text-xs leading-relaxed ${isStderr ? "text-red-400" : "text-muted-foreground"}`}>
        <AnsiOutput isError={isStderr}>{text}</AnsiOutput>
      </div>
    );
  }

  if (output.output_type === "error") {
    const tb = output.traceback?.join("\n") ?? `${output.ename}: ${output.evalue}`;
    return (
      <div className="rounded-md bg-red-950/30 p-2 font-mono text-xs leading-relaxed text-red-400">
        <AnsiOutput isError>{tb}</AnsiOutput>
      </div>
    );
  }

  if (output.output_type === "execute_result" || output.output_type === "display_data") {
    const data = output.data ?? {};
    const textPlain = data["text/plain"] as string | undefined;
    if (textPlain) {
      return (
        <div className="font-mono text-xs leading-relaxed text-blue-300">
          <AnsiOutput>{textPlain}</AnsiOutput>
        </div>
      );
    }
    return (
      <div className="font-mono text-xs text-muted-foreground italic">
        [display_data: {Object.keys(data).join(", ")}]
      </div>
    );
  }

  return null;
}
