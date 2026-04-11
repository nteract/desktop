/**
 * Accordion widget - collapsible panel container.
 *
 * Maps to ipywidgets AccordionModel. Displays children as collapsible panels.
 */

import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from "@/components/ui/accordion";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import {
  parseModelRef,
  useWidgetModelValue,
  useWidgetStoreRequired,
} from "../widget-store-context";
import { WidgetView } from "../widget-view";

export function AccordionWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const children = useWidgetModelValue<string[]>(modelId, "children");
  const titles = useWidgetModelValue<string[]>(modelId, "_titles") ?? [];
  const selectedIndex = useWidgetModelValue<number | null>(modelId, "selected_index");

  const handleValueChange = (value: string) => {
    // Convert string value back to index (or null if collapsed)
    const index = value !== "" ? parseInt(value, 10) : null;
    sendUpdate(modelId, { selected_index: index });
  };

  return (
    <Accordion
      type="single"
      collapsible
      value={selectedIndex !== null ? String(selectedIndex) : ""}
      onValueChange={handleValueChange}
      className={cn("w-full", className)}
      data-widget-id={modelId}
      data-widget-type="Accordion"
    >
      {children?.map((childRef, index) => {
        const childId = parseModelRef(childRef);
        if (!childId) return null;

        return (
          <AccordionItem key={childId} value={String(index)}>
            <AccordionTrigger>{titles[index] ?? `Panel ${index + 1}`}</AccordionTrigger>
            <AccordionContent>
              <WidgetView modelId={childId} />
            </AccordionContent>
          </AccordionItem>
        );
      })}
    </Accordion>
  );
}

export default AccordionWidget;
