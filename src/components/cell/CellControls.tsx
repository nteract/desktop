import {
  ArrowDown,
  ArrowDownToLine,
  ArrowUp,
  ArrowUpToLine,
  ChevronDown,
  ChevronUp,
  Eraser,
  Eye,
  EyeOff,
  MoreVertical,
  X,
} from "lucide-react";
import type React from "react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";

interface CellControlsProps {
  sourceVisible: boolean;
  onDeleteCell: () => void;
  onClearOutputs: () => void;
  hasOutputs: boolean;
  toggleSourceVisibility: () => void;
  playButton?: React.ReactNode;
  onMoveUp?: () => void;
  onMoveDown?: () => void;
  onMoveToTop?: () => void;
  onMoveToBottom?: () => void;
  canMoveUp?: boolean;
  canMoveDown?: boolean;
  onDeleteAllBelow?: () => void;
  hasCellsBelow?: boolean;
  /** Optional: Whether AI context is visible for this cell */
  aiContextVisible?: boolean;
  /** Optional: Whether context selection mode is active */
  contextSelectionMode?: boolean;
  /** Optional: Toggle AI context visibility */
  toggleAiContextVisibility?: () => void;
  /** Force controls to be visible, bypassing hover behavior */
  forceVisible?: boolean;
  className?: string;
}

export const CellControls: React.FC<CellControlsProps> = ({
  sourceVisible,
  aiContextVisible = false,
  contextSelectionMode = false,
  onDeleteCell,
  onClearOutputs,
  hasOutputs,
  toggleSourceVisibility,
  toggleAiContextVisibility,
  playButton,
  onMoveUp,
  onMoveDown,
  onMoveToTop,
  onMoveToBottom,
  canMoveUp = false,
  canMoveDown = false,
  onDeleteAllBelow,
  hasCellsBelow = false,
  forceVisible,
  className,
}) => {
  return (
    <div
      data-slot="cell-controls"
      className={cn(
        "cell-controls flex items-center gap-0.5 transition-opacity",
        forceVisible ? "opacity-100" : "opacity-100 sm:opacity-0 sm:group-hover:opacity-100",
        className,
      )}
    >
      {/* Mobile Play Button */}
      {playButton}

      {/* Source Visibility Toggle */}
      <Button
        variant="ghost"
        size="sm"
        onClick={toggleSourceVisibility}
        className={cn(
          "hover:bg-muted/80 h-8 w-8 p-0 sm:h-7 sm:w-7",
          !sourceVisible && "text-muted-foreground/60",
        )}
        title={sourceVisible ? "Hide source" : "Show source"}
      >
        {sourceVisible ? (
          <ChevronUp className="h-4 w-4 sm:h-3 sm:w-3" />
        ) : (
          <ChevronDown className="h-4 w-4 sm:h-3 sm:w-3" />
        )}
      </Button>

      {/* Context Selection Mode Button */}
      {contextSelectionMode && toggleAiContextVisibility && (
        <Button
          variant="ghost"
          size="sm"
          onClick={toggleAiContextVisibility}
          className={cn(
            "hover:bg-muted/80 h-8 w-8 p-0 sm:h-7 sm:w-7",
            aiContextVisible ? "text-purple-600" : "text-gray-500",
          )}
          title={aiContextVisible ? "Hide from AI context" : "Show in AI context"}
        >
          {aiContextVisible ? (
            <Eye className="h-4 w-4 sm:h-3 sm:w-3" />
          ) : (
            <EyeOff className="h-4 w-4 sm:h-3 sm:w-3" />
          )}
        </Button>
      )}

      {/* Desktop-only controls */}
      <div className="desktop-controls hidden items-center gap-0.5 sm:flex">
        {/* Separator */}
        <div className="bg-border/50 mx-1 h-4 w-px" />
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="sm"
              className="hover:bg-muted/80 h-7 w-7 p-0"
              title="More options"
            >
              <MoreVertical className="h-3 w-3" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {onMoveToTop && (
              <DropdownMenuItem onClick={onMoveToTop} disabled={!canMoveUp}>
                <ArrowUpToLine className="mr-2 h-4 w-4" />
                <span>Move to top</span>
              </DropdownMenuItem>
            )}
            {onMoveUp && (
              <DropdownMenuItem onClick={onMoveUp} disabled={!canMoveUp}>
                <ArrowUp className="mr-2 h-4 w-4" />
                <span>Move up</span>
              </DropdownMenuItem>
            )}
            {onMoveDown && (
              <DropdownMenuItem onClick={onMoveDown} disabled={!canMoveDown}>
                <ArrowDown className="mr-2 h-4 w-4" />
                <span>Move down</span>
              </DropdownMenuItem>
            )}
            {onMoveToBottom && (
              <DropdownMenuItem onClick={onMoveToBottom} disabled={!canMoveDown}>
                <ArrowDownToLine className="mr-2 h-4 w-4" />
                <span>Move to bottom</span>
              </DropdownMenuItem>
            )}
            {(onMoveUp || onMoveDown || onMoveToTop || onMoveToBottom) && <DropdownMenuSeparator />}
            <DropdownMenuItem onClick={onClearOutputs} disabled={!hasOutputs}>
              <Eraser className="mr-2 h-4 w-4" />
              <span>Clear outputs</span>
            </DropdownMenuItem>
            <DropdownMenuItem onClick={onDeleteCell} variant="destructive">
              <X className="mr-2 h-4 w-4" />
              <span>Delete cell</span>
            </DropdownMenuItem>
            {onDeleteAllBelow && hasCellsBelow && (
              <DropdownMenuItem onClick={onDeleteAllBelow} variant="destructive">
                <X className="mr-2 h-4 w-4" />
                <span>Delete all cells below</span>
                <ArrowDownToLine className="ml-2 h-4 w-4" />
              </DropdownMenuItem>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </div>
  );
};
