/**
 * Rewrite markdown and inline HTML asset refs to blob URLs.
 *
 * Supports:
 * - Markdown image syntax: ![alt](path), ![alt](<path>), ![alt](path "title")
 * - Reference-style markdown images: `![alt][id]` with `[id]: path`
 * - Inline HTML image tags: <img src="path">
 */
export function rewriteMarkdownAssetRefs(
  source: string,
  resolvedAssets: Record<string, string> | undefined,
  blobPort: number | null,
): string {
  if (
    !resolvedAssets ||
    blobPort === null ||
    Object.keys(resolvedAssets).length === 0
  ) {
    return source;
  }

  let result = source;

  for (const [assetRef, hash] of Object.entries(resolvedAssets)) {
    const blobUrl = `http://127.0.0.1:${blobPort}/blob/${hash}`;
    const escapedRef = assetRef.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const markdownImageSuffix = `((?:[ \\t]+(?:"[^"]*"|'[^']*'|\\([^\\n)]*\\)))?[ \\t]*\\))`;

    result = result
      .replace(
        new RegExp(
          `(!\\[[^\\]]*\\]\\([ \\t]*)<?${escapedRef}>?${markdownImageSuffix}`,
          "g",
        ),
        `$1${blobUrl}$2`,
      )
      .replace(
        new RegExp(
          `(^[ \\t]{0,3}\\[[^\\]]+\\]:[ \\t]*<?)${escapedRef}(>?((?:[ \\t]+(?:"[^"]*"|'[^']*'|\\([^\\n)]*\\)))?)[ \\t]*$)`,
          "gm",
        ),
        `$1${blobUrl}$2`,
      )
      .replace(
        new RegExp(`(\\bsrc\\s*=\\s*")${escapedRef}(")`, "gi"),
        `$1${blobUrl}$2`,
      )
      .replace(
        new RegExp(`(\\bsrc\\s*=\\s*')${escapedRef}(')`, "gi"),
        `$1${blobUrl}$2`,
      )
      .replace(
        new RegExp(`(\\bsrc\\s*=\\s*)${escapedRef}(?=[\\s>])`, "gi"),
        `$1${blobUrl}`,
      );
  }

  return result;
}
