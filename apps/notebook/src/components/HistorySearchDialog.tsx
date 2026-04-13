import { memo, useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { StaticCodeBlock } from "@/components/editor/static-highlight";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { isDarkMode, useColorTheme } from "@/lib/dark-mode";
import { type HistoryEntry, useHistorySearch } from "../hooks/useHistorySearch";

interface HistorySearchDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelect: (source: string) => void;
}

/** Syntax-highlighted code preview for history entries (memoized to avoid re-renders) */
const CodePreview = memo(function CodePreview({
  code,
  maxLines = 8,
}: {
  code: string;
  maxLines?: number;
}) {
  const isDark = isDarkMode();
  const colorTheme = (useColorTheme() ?? "classic") as "classic" | "cream";

  // Truncate to maxLines
  const lines = code.split("\n");
  const truncated = lines.length > maxLines;
  const displayCode = truncated ? `${lines.slice(0, maxLines).join("\n")}\n...` : code;

  return (
    <StaticCodeBlock
      code={displayCode}
      language="python"
      isDark={isDark}
      colorTheme={colorTheme}
      className="!m-0 !p-2 !text-xs !leading-[1.4] !bg-transparent overflow-hidden"
    />
  );
});

export function HistorySearchDialog({ open, onOpenChange, onSelect }: HistorySearchDialogProps) {
  const { entries, isLoading, error, searchHistory, clearEntries } = useHistorySearch();
  const [searchValue, setSearchValue] = useState("");
  // Defer the search value for filtering to keep input responsive
  const deferredSearchValue = useDeferredValue(searchValue);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Fetch initial history (Tail) when dialog opens
  useEffect(() => {
    if (open) {
      searchHistory(); // No pattern = Tail request
      setSearchValue("");
    } else {
      clearEntries();
    }
  }, [open, searchHistory, clearEntries]);

  // Debounced kernel search when user types
  useEffect(() => {
    if (!open) return;

    if (debounceRef.current) {
      clearTimeout(debounceRef.current);
    }

    debounceRef.current = setTimeout(() => {
      // Only call kernel search if there's a non-empty search term
      if (searchValue.trim()) {
        searchHistory(searchValue.trim());
      } else {
        // Empty search = fetch tail again
        searchHistory();
      }
    }, 300);

    return () => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
    };
  }, [searchValue, open, searchHistory]);

  // Filter client-side using deferred value to keep input responsive
  const filteredEntries = useMemo(() => {
    if (!deferredSearchValue.trim()) {
      return entries;
    }
    const search = deferredSearchValue.toLowerCase();
    return entries.filter((entry) => entry.source.toLowerCase().includes(search));
  }, [entries, deferredSearchValue]);

  const handleSelect = useCallback(
    (entry: HistoryEntry) => {
      onSelect(entry.source);
      onOpenChange(false);
    },
    [onSelect, onOpenChange],
  );

  // Determine what empty message to show
  // Only show loading/empty messages when there are no entries to display
  const emptyMessage = useMemo(() => {
    if (error) {
      if (error.includes("No kernel running")) {
        return "Start a kernel to search history.";
      }
      return `Error: ${error}`;
    }
    // Show loading message only when we have nothing to display
    if (isLoading && filteredEntries.length === 0) {
      return "Searching history...";
    }
    if (entries.length === 0) {
      return "No history found.";
    }
    if (filteredEntries.length === 0) {
      return "No matching history.";
    }
    return null;
  }, [error, isLoading, entries.length, filteredEntries.length]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogHeader className="sr-only">
        <DialogTitle>History Search</DialogTitle>
        <DialogDescription>Search through your IPython command history (Ctrl+R)</DialogDescription>
      </DialogHeader>
      <DialogContent className="overflow-hidden p-0 max-w-2xl" showCloseButton={false}>
        <Command
          shouldFilter={false}
          className="[&_[cmdk-group-heading]]:text-muted-foreground **:data-[slot=command-input-wrapper]:h-12 [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group]]:px-2 [&_[cmdk-group]:not([hidden])_~[cmdk-group]]:pt-0 [&_[cmdk-input-wrapper]_svg]:h-5 [&_[cmdk-input-wrapper]_svg]:w-5 [&_[cmdk-input]]:h-12 [&_[cmdk-item]]:px-2 [&_[cmdk-item]]:py-1 [&_[cmdk-item]_svg]:h-5 [&_[cmdk-item]_svg]:w-5"
        >
          <CommandInput
            placeholder="Search history..."
            value={searchValue}
            onValueChange={setSearchValue}
          />
          <CommandList className="max-h-[400px]">
            {emptyMessage ? (
              <CommandEmpty>{emptyMessage}</CommandEmpty>
            ) : (
              <CommandGroup heading={`History${isLoading ? " (searching...)" : ""}`}>
                {filteredEntries.map((entry, index) => (
                  <CommandItem
                    key={`${entry.session}-${entry.line}-${index}`}
                    value={`${entry.session}-${entry.line}`}
                    onSelect={() => handleSelect(entry)}
                    className="cursor-pointer"
                  >
                    <div className="w-full overflow-hidden rounded border border-border/50">
                      <CodePreview code={entry.source} maxLines={6} />
                    </div>
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
          </CommandList>
        </Command>
      </DialogContent>
    </Dialog>
  );
}
