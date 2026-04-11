import type { ComponentType } from "react";

export interface WidgetComponentProps {
  /** The model ID (comm_id) of the widget */
  modelId: string;
  /** Optional className for styling */
  className?: string;
}

/**
 * Registry of model names to widget components.
 * Components are loaded lazily to avoid bundling unused widgets.
 */
export const WIDGET_REGISTRY: Record<string, ComponentType<WidgetComponentProps>> = {};

/**
 * Register a widget component for a model name.
 */
export function registerWidget(
  modelName: string,
  component: ComponentType<WidgetComponentProps>,
): void {
  WIDGET_REGISTRY[modelName] = component;
}

/**
 * Get the component for a model name, if registered.
 */
export function getWidgetComponent(
  modelName: string,
): ComponentType<WidgetComponentProps> | undefined {
  return WIDGET_REGISTRY[modelName];
}

/**
 * Check if a model name has a registered component.
 */
export function hasWidgetComponent(modelName: string): boolean {
  return modelName in WIDGET_REGISTRY;
}
