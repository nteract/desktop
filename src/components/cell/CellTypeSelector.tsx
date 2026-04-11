import { Bot, ChevronDown, Code, Database, FileText } from "lucide-react";
import { type CellType, cellTypeStyles } from "@/components/cell/CellTypeButton";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";

const allCellTypes: CellType[] = ["code", "markdown", "sql", "ai"];

const cellTypeIcons = {
  code: Code,
  markdown: FileText,
  sql: Database,
  ai: Bot,
};

const cellTypeLabels = {
  code: "Code",
  markdown: "Markdown",
  sql: "SQL",
  ai: "AI",
};

const cellTypeMenuItemStyles = {
  code: "text-gray-500 dark:text-gray-400 [&_svg]:text-gray-500 dark:[&_svg]:text-gray-400 focus:bg-gray-100 dark:focus:bg-gray-800 focus:text-gray-700 dark:focus:text-gray-200 focus:[&_svg]:text-gray-700 dark:focus:[&_svg]:text-gray-200",
  markdown:
    "text-yellow-600/70 [&_svg]:text-yellow-600/70 focus:bg-yellow-50 dark:focus:bg-yellow-900/30 focus:text-yellow-600 dark:focus:text-yellow-500 focus:[&_svg]:text-yellow-600 dark:focus:[&_svg]:text-yellow-500",
  sql: "text-blue-600/70 [&_svg]:text-blue-600/70 focus:bg-blue-50 dark:focus:bg-blue-900/30 focus:text-blue-600 dark:focus:text-blue-500 focus:[&_svg]:text-blue-600 dark:focus:[&_svg]:text-blue-500",
  ai: "text-purple-600/70 [&_svg]:text-purple-600/70 focus:bg-purple-50 dark:focus:bg-purple-900/30 focus:text-purple-600 dark:focus:text-purple-500 focus:[&_svg]:text-purple-600 dark:focus:[&_svg]:text-purple-500",
};

interface CellTypeSelectorProps {
  currentType: CellType;
  onTypeChange: (type: CellType) => void;
  enabledTypes?: CellType[];
}

export function CellTypeSelector({
  currentType,
  onTypeChange,
  enabledTypes = allCellTypes,
}: CellTypeSelectorProps) {
  const Icon = cellTypeIcons[currentType];
  const availableTypes = allCellTypes.filter((type) => enabledTypes.includes(type));

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          data-slot="cell-type-selector"
          variant="outline"
          size="sm"
          className={cn(cellTypeStyles[currentType], "flex items-center gap-1.5")}
        >
          <Icon className="h-3 w-3" />
          {cellTypeLabels[currentType]}
          <ChevronDown className="h-3 w-3 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start">
        {availableTypes.map((type) => {
          const TypeIcon = cellTypeIcons[type];
          return (
            <DropdownMenuItem
              key={type}
              onClick={() => onTypeChange(type)}
              className={cn(
                "flex items-center gap-2",
                cellTypeMenuItemStyles[type],
                type === currentType && "bg-accent",
              )}
            >
              <TypeIcon className="h-4 w-4" />
              {cellTypeLabels[type]}
            </DropdownMenuItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
