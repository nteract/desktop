import { open } from "@tauri-apps/plugin-shell";

export async function openUrl(url: string): Promise<void> {
  await open(url);
}
