/**
 * Tab widget - tabbed panel container.
 *
 * Maps to ipywidgets TabModel. Displays children as tabbed panels.
 */

import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import {
  parseModelRef,
  useWidgetModelValue,
  useWidgetStoreRequired,
} from "../widget-store-context";
import { WidgetView } from "../widget-view";

export function TabWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const children = useWidgetModelValue<string[]>(modelId, "children");
  const titles = useWidgetModelValue<string[]>(modelId, "_titles") ?? [];
  const selectedIndex = useWidgetModelValue<number>(modelId, "selected_index") ?? 0;

  const handleValueChange = (value: string) => {
    sendUpdate(modelId, { selected_index: parseInt(value, 10) });
  };

  return (
    <Tabs
      value={String(selectedIndex)}
      onValueChange={handleValueChange}
      className={cn("w-full", className)}
      data-widget-id={modelId}
      data-widget-type="Tab"
    >
      <TabsList>
        {children?.map((_, index) => (
          <TabsTrigger key={index} value={String(index)}>
            {titles[index] ?? `Tab ${index + 1}`}
          </TabsTrigger>
        ))}
      </TabsList>
      {children?.map((childRef, index) => {
        const childId = parseModelRef(childRef);
        if (!childId) return null;

        return (
          <TabsContent key={childId} value={String(index)}>
            <WidgetView modelId={childId} />
          </TabsContent>
        );
      })}
    </Tabs>
  );
}

export default TabWidget;
