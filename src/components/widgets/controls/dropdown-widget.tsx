/**
 * Dropdown widget - renders a native select dropdown.
 *
 * Maps to ipywidgets DropdownModel.
 *
 * Uses native HTML <select> instead of Radix UI Select because Radix
 * uses Portal which doesn't work in sandboxed iframes (no allow-same-origin).
 * See: https://github.com/runtimed/runt/issues/62
 */

import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function DropdownWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const options = useWidgetModelValue<string[]>(modelId, "_options_labels") ?? [];
  const index = useWidgetModelValue<number | null>(modelId, "index");
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;

  // Convert index to string value for select
  const value = index !== null && index !== undefined && index >= 0 ? String(index) : "";

  const handleChange = (event: React.ChangeEvent<HTMLSelectElement>) => {
    const newIndex = parseInt(event.target.value, 10);
    if (!Number.isNaN(newIndex)) {
      sendUpdate(modelId, { index: newIndex });
    }
  };

  return (
    <div
      className={cn("flex items-center gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="Dropdown"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <select
        value={value}
        onChange={handleChange}
        disabled={disabled}
        className={cn(
          // Base styles matching shadcn SelectTrigger
          "h-9 w-48 appearance-none rounded-md border border-input px-3 py-2 text-sm",
          "bg-transparent shadow-xs transition-[color,box-shadow] outline-none",
          // Dark mode background
          "dark:bg-input/30 dark:hover:bg-input/50",
          // Focus styles
          "focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50",
          // Disabled state
          "disabled:cursor-not-allowed disabled:opacity-50",
          // Custom dropdown arrow using background image (chevron-down)
          "bg-[length:16px_16px] bg-[right_8px_center] bg-no-repeat",
          "bg-[url('data:image/svg+xml;charset=utf-8,%3Csvg%20xmlns%3D%22http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%22%20width%3D%2224%22%20height%3D%2224%22%20viewBox%3D%220%200%2024%2024%22%20fill%3D%22none%22%20stroke%3D%22%239ca3af%22%20stroke-width%3D%222%22%20stroke-linecap%3D%22round%22%20stroke-linejoin%3D%22round%22%3E%3Cpath%20d%3D%22m6%209%206%206%206-6%22%2F%3E%3C%2Fsvg%3E')]",
          "pr-9", // Extra padding for the arrow
        )}
      >
        {options.map((option, idx) => (
          <option key={idx} value={String(idx)}>
            {option}
          </option>
        ))}
      </select>
    </div>
  );
}

export default DropdownWidget;
