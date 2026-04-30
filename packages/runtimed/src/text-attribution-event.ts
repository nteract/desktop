import type { TextAttribution } from "./handle";

export const TEXT_ATTRIBUTION_EVENT_TYPE = "text_attribution" as const;

export interface TextAttributionEvent {
  type: typeof TEXT_ATTRIBUTION_EVENT_TYPE;
  attributions: TextAttribution[];
}

export function createTextAttributionEvent(
  attributions: TextAttribution[],
): TextAttributionEvent {
  return {
    type: TEXT_ATTRIBUTION_EVENT_TYPE,
    attributions,
  };
}

export function isTextAttributionEvent(payload: unknown): payload is TextAttributionEvent {
  if (
    typeof payload !== "object" ||
    payload === null ||
    (payload as { type?: unknown }).type !== TEXT_ATTRIBUTION_EVENT_TYPE
  ) {
    return false;
  }

  const attributions = (payload as { attributions?: unknown }).attributions;
  return Array.isArray(attributions) && attributions.every(isTextAttribution);
}

function isTextAttribution(payload: unknown): payload is TextAttribution {
  if (typeof payload !== "object" || payload === null) {
    return false;
  }

  const attribution = payload as Partial<Record<keyof TextAttribution, unknown>>;
  return (
    typeof attribution.cell_id === "string" &&
    typeof attribution.index === "number" &&
    Number.isFinite(attribution.index) &&
    typeof attribution.text === "string" &&
    typeof attribution.deleted === "number" &&
    Number.isFinite(attribution.deleted) &&
    Array.isArray(attribution.actors) &&
    attribution.actors.every((actor) => typeof actor === "string")
  );
}
