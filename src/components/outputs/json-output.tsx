import { ChevronRightIcon } from "lucide-react";
import { createContext, type HTMLAttributes, useContext, useState } from "react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { cn } from "@/lib/utils";

interface JsonViewerContextType {
  expandedPaths: Set<string>;
  togglePath: (path: string) => void;
  displayDataTypes: boolean;
}

const JsonViewerContext = createContext<JsonViewerContextType>({
  expandedPaths: new Set(),
  togglePath: () => undefined,
  displayDataTypes: false,
});

interface JsonOutputProps {
  /**
   * The JSON data to render. Can be any JSON-serializable value.
   */
  data: unknown;
  /**
   * Collapse nested objects beyond this depth.
   * Set to `false` to expand all, or a number for depth limit.
   * @default false
   */
  collapsed?: boolean | number;
  /**
   * Show data types alongside values
   * @default false
   */
  displayDataTypes?: boolean;
  /**
   * Additional CSS classes
   */
  className?: string;
}

/**
 * JsonOutput component for rendering JSON data in notebook outputs
 *
 * Displays JSON data in an interactive, expandable tree view.
 * Useful for inspecting complex data structures, API responses,
 * and object representations from Jupyter kernels.
 */
export function JsonOutput({
  data,
  collapsed = false,
  displayDataTypes = false,
  className = "",
}: JsonOutputProps) {
  // Calculate initial expanded paths based on collapsed prop
  const getInitialExpanded = (): Set<string> => {
    if (collapsed === false) {
      // Expand all - collect all paths
      return collectAllPaths(data, "$");
    }
    if (collapsed === true) {
      // Collapse all
      return new Set();
    }
    // Expand up to specified depth
    return collectPathsToDepth(data, "$", collapsed);
  };

  const [expandedPaths, setExpandedPaths] = useState<Set<string>>(getInitialExpanded);

  const togglePath = (path: string) => {
    const newExpanded = new Set(expandedPaths);
    if (newExpanded.has(path)) {
      newExpanded.delete(path);
    } else {
      newExpanded.add(path);
    }
    setExpandedPaths(newExpanded);
  };

  if (data === undefined || data === null) {
    return (
      <div
        data-slot="json-output"
        className={cn("not-prose py-2 font-mono text-sm text-muted-foreground", className)}
      >
        <JsonPrimitive value={data} displayDataTypes={displayDataTypes} />
      </div>
    );
  }

  return (
    <JsonViewerContext.Provider value={{ expandedPaths, togglePath, displayDataTypes }}>
      <div data-slot="json-output" className={cn("not-prose py-2", className)}>
        <div className="rounded-lg border bg-background p-3 font-mono text-sm">
          <JsonValue value={data} path="$" keyName={null} isLast />
        </div>
      </div>
    </JsonViewerContext.Provider>
  );
}

function collectAllPaths(value: unknown, path: string): Set<string> {
  const paths = new Set<string>();
  if (value && typeof value === "object") {
    paths.add(path);
    if (Array.isArray(value)) {
      value.forEach((item, index) => {
        const childPaths = collectAllPaths(item, `${path}[${index}]`);
        childPaths.forEach((p) => paths.add(p));
      });
    } else {
      Object.entries(value).forEach(([key, val]) => {
        const childPaths = collectAllPaths(val, `${path}.${key}`);
        childPaths.forEach((p) => paths.add(p));
      });
    }
  }
  return paths;
}

function collectPathsToDepth(
  value: unknown,
  path: string,
  maxDepth: number,
  currentDepth = 0,
): Set<string> {
  const paths = new Set<string>();
  if (value && typeof value === "object" && currentDepth < maxDepth) {
    paths.add(path);
    if (Array.isArray(value)) {
      value.forEach((item, index) => {
        const childPaths = collectPathsToDepth(
          item,
          `${path}[${index}]`,
          maxDepth,
          currentDepth + 1,
        );
        childPaths.forEach((p) => paths.add(p));
      });
    } else {
      Object.entries(value).forEach(([key, val]) => {
        const childPaths = collectPathsToDepth(val, `${path}.${key}`, maxDepth, currentDepth + 1);
        childPaths.forEach((p) => paths.add(p));
      });
    }
  }
  return paths;
}

interface JsonValueProps {
  value: unknown;
  path: string;
  keyName: string | null;
  isLast: boolean;
}

function JsonValue({ value, path, keyName, isLast }: JsonValueProps) {
  const { displayDataTypes } = useContext(JsonViewerContext);

  if (value === null || value === undefined) {
    return (
      <JsonEntry keyName={keyName} isLast={isLast}>
        <JsonPrimitive value={value} displayDataTypes={displayDataTypes} />
      </JsonEntry>
    );
  }

  if (Array.isArray(value)) {
    return <JsonArray value={value} path={path} keyName={keyName} isLast={isLast} />;
  }

  if (typeof value === "object") {
    return (
      <JsonObject
        value={value as Record<string, unknown>}
        path={path}
        keyName={keyName}
        isLast={isLast}
      />
    );
  }

  return (
    <JsonEntry keyName={keyName} isLast={isLast}>
      <JsonPrimitive value={value} displayDataTypes={displayDataTypes} />
    </JsonEntry>
  );
}

interface JsonEntryProps extends HTMLAttributes<HTMLDivElement> {
  keyName: string | null;
  isLast: boolean;
}

function JsonEntry({ keyName, isLast, children, className, ...props }: JsonEntryProps) {
  return (
    <div className={cn("flex items-baseline gap-1", className)} {...props}>
      {keyName !== null && (
        <>
          <span className="text-foreground">&quot;{keyName}&quot;</span>
          <span className="text-muted-foreground">:</span>
        </>
      )}
      {children}
      {!isLast && <span className="text-muted-foreground">,</span>}
    </div>
  );
}

interface JsonObjectProps {
  value: Record<string, unknown>;
  path: string;
  keyName: string | null;
  isLast: boolean;
}

function JsonObject({ value, path, keyName, isLast }: JsonObjectProps) {
  const { expandedPaths, togglePath, displayDataTypes } = useContext(JsonViewerContext);
  const isExpanded = expandedPaths.has(path);
  const entries = Object.entries(value);
  const isEmpty = entries.length === 0;

  if (isEmpty) {
    return (
      <JsonEntry keyName={keyName} isLast={isLast}>
        <span className="text-muted-foreground">{"{}"}</span>
        {displayDataTypes && <JsonTypeLabel type="object" />}
      </JsonEntry>
    );
  }

  return (
    <Collapsible open={isExpanded} onOpenChange={() => togglePath(path)}>
      <div>
        <CollapsibleTrigger asChild>
          <button
            type="button"
            className="group flex items-baseline gap-1 text-left hover:bg-muted/50 rounded px-1 -mx-1"
          >
            <ChevronRightIcon
              className={cn(
                "size-3 shrink-0 text-muted-foreground transition-transform mt-1 self-start",
                isExpanded && "rotate-90",
              )}
            />
            {keyName !== null && (
              <>
                <span className="text-foreground">&quot;{keyName}&quot;</span>
                <span className="text-muted-foreground">:</span>
              </>
            )}
            <span className="text-muted-foreground">{"{"}</span>
            {!isExpanded && (
              <>
                <span className="text-muted-foreground/60">...</span>
                <span className="text-muted-foreground">{"}"}</span>
                {displayDataTypes && <JsonTypeLabel type="object" />}
                <span className="text-muted-foreground/60 text-xs ml-1">
                  {entries.length} {entries.length === 1 ? "key" : "keys"}
                </span>
              </>
            )}
          </button>
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div className="ml-4 border-l border-border pl-3">
            {entries.map(([key, val], index) => (
              <JsonValue
                key={key}
                value={val}
                path={`${path}.${key}`}
                keyName={key}
                isLast={index === entries.length - 1}
              />
            ))}
          </div>
          <div className="flex items-baseline gap-1">
            <span className="size-3 shrink-0" />
            <span className="text-muted-foreground">{"}"}</span>
            {displayDataTypes && <JsonTypeLabel type="object" />}
            {!isLast && <span className="text-muted-foreground">,</span>}
          </div>
        </CollapsibleContent>
      </div>
    </Collapsible>
  );
}

interface JsonArrayProps {
  value: unknown[];
  path: string;
  keyName: string | null;
  isLast: boolean;
}

function JsonArray({ value, path, keyName, isLast }: JsonArrayProps) {
  const { expandedPaths, togglePath, displayDataTypes } = useContext(JsonViewerContext);
  const isExpanded = expandedPaths.has(path);
  const isEmpty = value.length === 0;

  if (isEmpty) {
    return (
      <JsonEntry keyName={keyName} isLast={isLast}>
        <span className="text-muted-foreground">[]</span>
        {displayDataTypes && <JsonTypeLabel type="array" />}
      </JsonEntry>
    );
  }

  return (
    <Collapsible open={isExpanded} onOpenChange={() => togglePath(path)}>
      <div>
        <CollapsibleTrigger asChild>
          <button
            type="button"
            className="group flex items-baseline gap-1 text-left hover:bg-muted/50 rounded px-1 -mx-1"
          >
            <ChevronRightIcon
              className={cn(
                "size-3 shrink-0 text-muted-foreground transition-transform mt-1 self-start",
                isExpanded && "rotate-90",
              )}
            />
            {keyName !== null && (
              <>
                <span className="text-foreground">&quot;{keyName}&quot;</span>
                <span className="text-muted-foreground">:</span>
              </>
            )}
            <span className="text-muted-foreground">[</span>
            {!isExpanded && (
              <>
                <span className="text-muted-foreground/60">...</span>
                <span className="text-muted-foreground">]</span>
                {displayDataTypes && <JsonTypeLabel type="array" />}
                <span className="text-muted-foreground/60 text-xs ml-1">
                  {value.length} {value.length === 1 ? "item" : "items"}
                </span>
              </>
            )}
          </button>
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div className="ml-4 border-l border-border pl-3">
            {value.map((item, index) => (
              <JsonValue
                key={index}
                value={item}
                path={`${path}[${index}]`}
                keyName={null}
                isLast={index === value.length - 1}
              />
            ))}
          </div>
          <div className="flex items-baseline gap-1">
            <span className="size-3 shrink-0" />
            <span className="text-muted-foreground">]</span>
            {displayDataTypes && <JsonTypeLabel type="array" />}
            {!isLast && <span className="text-muted-foreground">,</span>}
          </div>
        </CollapsibleContent>
      </div>
    </Collapsible>
  );
}

interface JsonPrimitiveProps {
  value: unknown;
  displayDataTypes: boolean;
}

function JsonPrimitive({ value, displayDataTypes }: JsonPrimitiveProps) {
  if (value === null) {
    return (
      <span className="inline-flex items-baseline gap-1">
        <span className="text-orange-600 dark:text-orange-400">null</span>
        {displayDataTypes && <JsonTypeLabel type="null" />}
      </span>
    );
  }

  if (value === undefined) {
    return (
      <span className="inline-flex items-baseline gap-1">
        <span className="text-muted-foreground">undefined</span>
        {displayDataTypes && <JsonTypeLabel type="undefined" />}
      </span>
    );
  }

  if (typeof value === "string") {
    return (
      <span className="inline-flex items-baseline gap-1">
        <span className="text-green-600 dark:text-green-400">&quot;{value}&quot;</span>
        {displayDataTypes && <JsonTypeLabel type="string" />}
      </span>
    );
  }

  if (typeof value === "number") {
    return (
      <span className="inline-flex items-baseline gap-1">
        <span className="text-blue-600 dark:text-blue-400">{value}</span>
        {displayDataTypes && <JsonTypeLabel type="number" />}
      </span>
    );
  }

  if (typeof value === "boolean") {
    return (
      <span className="inline-flex items-baseline gap-1">
        <span className="text-purple-600 dark:text-purple-400">{value ? "true" : "false"}</span>
        {displayDataTypes && <JsonTypeLabel type="boolean" />}
      </span>
    );
  }

  // Fallback for other types
  return <span className="text-muted-foreground">{String(value)}</span>;
}

function JsonTypeLabel({ type }: { type: string }) {
  return <span className="text-xs text-muted-foreground/60 ml-1">{type}</span>;
}
