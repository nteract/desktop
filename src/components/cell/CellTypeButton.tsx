import { Bot, Code, Database, FileText, Plus } from "lucide-react";
import type { ComponentProps } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/** Supported cell types for notebook interfaces */
export type CellType = "code" | "markdown" | "sql" | "ai";

// Colocated cell type color styles that won't be affected by shadcn updates
export const cellTypeStyles = {
  code: "border-gray-300 dark:border-gray-500 focus-visible:border-gray-500 text-gray-600 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-800 hover:text-gray-600 focus:bg-gray-50 dark:focus:bg-gray-800 focus-visible:ring-gray-100 dark:focus-visible:ring-gray-700",
  markdown:
    "border-yellow-300 focus-visible:border-yellow-500 text-yellow-600 hover:bg-yellow-50 hover:text-yellow-600 focus:bg-yellow-50 focus-visible:ring-yellow-100",
  sql: "border-blue-300 focus-visible:border-blue-500 text-blue-600 hover:bg-blue-50 hover:text-blue-600 focus:bg-blue-50 focus-visible:ring-blue-100",
  ai: "border-purple-300 focus-visible:border-purple-500 text-purple-600 hover:bg-purple-50 hover:text-purple-600 focus:bg-purple-50 focus-visible:ring-purple-100",
};

interface CellTypeButtonProps extends Omit<ComponentProps<typeof Button>, "children"> {
  cellType: CellType;
  showIcon?: boolean;
  showPlus?: boolean;
  label?: string;
}

export function CellTypeButton({
  cellType,
  showIcon = true,
  showPlus = false,
  label,
  className,
  ...props
}: CellTypeButtonProps) {
  const icons = {
    code: Code,
    markdown: FileText,
    sql: Database,
    ai: Bot,
  };

  const defaultLabels = {
    code: "Code",
    markdown: "Markdown",
    sql: "SQL",
    ai: "AI",
  };

  const Icon = icons[cellType];

  return (
    <Button
      data-slot="cell-type-button"
      variant="outline"
      className={cn(cellTypeStyles[cellType], "flex items-center gap-1.5", className)}
      {...props}
    >
      {showPlus && <Plus className={props.size === "lg" ? "h-4 w-4" : "h-3 w-3"} />}
      {showIcon && <Icon className={props.size === "lg" ? "h-4 w-4" : "h-3 w-3"} />}
      {label || defaultLabels[cellType]}
    </Button>
  );
}

// Convenience components for specific cell types
export function CodeCellButton(props: Omit<CellTypeButtonProps, "cellType">) {
  return <CellTypeButton cellType="code" {...props} />;
}

export function MarkdownCellButton(props: Omit<CellTypeButtonProps, "cellType">) {
  return <CellTypeButton cellType="markdown" {...props} />;
}

export function SqlCellButton(props: Omit<CellTypeButtonProps, "cellType">) {
  return <CellTypeButton cellType="sql" {...props} />;
}

export function AiCellButton(props: Omit<CellTypeButtonProps, "cellType">) {
  return <CellTypeButton cellType="ai" {...props} />;
}
