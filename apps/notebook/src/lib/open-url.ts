import { open } from "@tauri-apps/plugin-shell";

const ALLOWED_PROTOCOLS = new Set(["http:", "https:", "mailto:", "tel:"]);

/**
 * Opens a URL in the system's default browser.
 * Only allows safe protocols (http, https, mailto, tel) since URLs
 * may originate from untrusted notebook content.
 */
export async function openUrl(url: string): Promise<void> {
  const normalized = url.trim();

  let parsed: URL;
  try {
    parsed = new URL(normalized);
  } catch {
    console.error("openUrl: refusing to open invalid URL", { url: normalized });
    return;
  }

  if (!ALLOWED_PROTOCOLS.has(parsed.protocol)) {
    console.error("openUrl: refusing to open URL with disallowed protocol", {
      url: normalized,
      protocol: parsed.protocol,
    });
    return;
  }

  try {
    await open(normalized);
  } catch (err) {
    console.error("openUrl: failed to open URL", {
      url: normalized,
      error: err,
    });
  }
}
