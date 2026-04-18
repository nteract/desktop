import type { NotebookHost } from "@nteract/notebook-host";

const ALLOWED_PROTOCOLS = new Set(["http:", "https:", "mailto:", "tel:"]);

let _host: NotebookHost | null = null;

/**
 * Register the `NotebookHost` instance for `openUrl`. Called once from
 * `main.tsx` right after the host is constructed — mirrors the logger
 * and transport setters. Using a module-level ref keeps call sites
 * (`openUrl(url)`) untouched so the migration to host-based URL opening
 * doesn't need to thread a host parameter through every markdown cell.
 */
export function setOpenUrlHost(host: NotebookHost | null): void {
  _host = host;
}

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

  if (!_host) {
    console.error("openUrl: host not initialized — dropping URL", {
      url: normalized,
    });
    return;
  }

  try {
    await _host.externalLinks.open(normalized);
  } catch (err) {
    console.error("openUrl: failed to open URL", {
      url: normalized,
      error: err,
    });
  }
}
