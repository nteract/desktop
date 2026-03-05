/**
 * Generate random user identity for presence.
 *
 * Creates fun animal-based names with matching Lucide icons and colors.
 * Used for anonymous presence display when agents/windows connect.
 */

import type { UserInfo } from "../types";

// Lucide animal icon names (avoiding bug icons for debug)
const ANIMALS = [
  "cat",
  "dog",
  "rabbit",
  "fish",
  "bird",
  "squirrel",
  "turtle",
  "snail",
] as const;

const ADJECTIVES = [
  "Swift",
  "Clever",
  "Calm",
  "Bright",
  "Happy",
  "Gentle",
  "Bold",
  "Wise",
] as const;

/**
 * Generate a random user identity for presence.
 * Creates a memorable name, matching icon, and consistent color.
 */
export function generateUserInfo(): UserInfo {
  const adj = ADJECTIVES[Math.floor(Math.random() * ADJECTIVES.length)];
  const animal = ANIMALS[Math.floor(Math.random() * ANIMALS.length)];

  // Generate consistent color from name (deterministic hue)
  const nameStr = `${adj}${animal}`;
  const hash = nameStr.split("").reduce((a, c) => a + c.charCodeAt(0), 0);
  const hue = hash % 360;

  // Capitalize animal for display name
  const displayAnimal = animal.charAt(0).toUpperCase() + animal.slice(1);

  return {
    name: `${adj} ${displayAnimal}`,
    icon: animal,
    color: `hsl(${hue}, 70%, 50%)`,
  };
}

/**
 * Get a consistent color for a given name.
 * Useful for showing the same color for a peer across sessions.
 */
export function colorFromName(name: string): string {
  const hash = name.split("").reduce((a, c) => a + c.charCodeAt(0), 0);
  const hue = hash % 360;
  return `hsl(${hue}, 70%, 50%)`;
}
