export { ImoCallout, type ImoCalloutData } from "./imo-callout";
export { ImoLayout, type ImoLayoutData } from "./imo-layout";
export { ImoStat, type ImoStatData } from "./imo-stat";

/**
 * MIME types for imo display objects.
 */
export const IMO_MIME_TYPES = {
  CALLOUT: "application/vnd.imo.callout+json",
  STAT: "application/vnd.imo.stat+json",
  HSTACK: "application/vnd.imo.hstack+json",
  VSTACK: "application/vnd.imo.vstack+json",
} as const;
