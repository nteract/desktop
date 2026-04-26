import { render, waitFor } from "@testing-library/react";
import React from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { MarkdownCell as MarkdownCellType } from "../../types";

let mockDarkMode = false;
let mockColorTheme: string | undefined;
let mockIsFocused = false;
const isolatedFrameProps: Array<Record<string, unknown>> = [];

const mockFrameHandle = {
  send: vi.fn(),
  render: vi.fn(),
  renderBatch: vi.fn(),
  eval: vi.fn(),
  installRenderer: vi.fn(),
  setTheme: vi.fn(),
  clear: vi.fn(),
  search: vi.fn(),
  searchNavigate: vi.fn(),
  isReady: true,
  isIframeReady: true,
};

vi.mock("@/lib/dark-mode", () => ({
  useDarkMode: () => mockDarkMode,
  useColorTheme: () => mockColorTheme,
}));

vi.mock("@/components/isolated/iframe-libraries", () => ({
  injectPluginsForMimes: vi.fn(async () => {}),
}));

vi.mock("@/components/isolated", async () => {
  const React = await import("react");

  const MockIsolatedFrame = React.forwardRef<
    typeof mockFrameHandle,
    Record<string, unknown> & { onReady?: () => void }
  >(function MockIsolatedFrame(props, ref) {
    isolatedFrameProps.push(props);
    React.useImperativeHandle(ref, () => mockFrameHandle);

    React.useEffect(() => {
      props.onReady?.();
    }, [props.onReady]);

    return <div data-testid="markdown-frame" />;
  });

  return {
    IsolatedFrame: MockIsolatedFrame,
  };
});

vi.mock("@/components/cell/CellContainer", () => ({
  CellContainer: ({ codeContent }: { codeContent: React.ReactNode }) => <div>{codeContent}</div>,
}));

vi.mock("@/components/editor/codemirror-editor", () => ({
  CodeMirrorEditor: React.forwardRef(function CodeMirrorEditor(_props, _ref) {
    return <div data-testid="markdown-editor" />;
  }),
}));

vi.mock("@/components/editor/remote-cursors", () => ({
  remoteCursorsExtension: () => [],
}));

vi.mock("@/components/editor/search-highlight", () => ({
  searchHighlight: () => [],
}));

vi.mock("@/components/editor/text-attribution", () => ({
  textAttributionExtension: () => [],
}));

vi.mock("../cell/CellPresenceIndicators", () => ({
  CellPresenceIndicators: () => null,
}));

vi.mock("../../contexts/PresenceContext", () => ({
  usePresenceContext: () => null,
}));

vi.mock("../../hooks/useCellKeyboardNavigation", () => ({
  useCellKeyboardNavigation: () => [],
}));

vi.mock("../../hooks/useCrdtBridge", () => ({
  useCrdtBridge: () => ({ extension: [] }),
}));

vi.mock("../../lib/blob-port", () => ({
  useBlobPort: () => null,
}));

vi.mock("../../lib/cell-ui-state", () => ({
  useIsCellFocused: () => mockIsFocused,
  useIsNextCellFromFocused: () => false,
  useIsPreviousCellFromFocused: () => false,
  useSearchQuery: () => "",
}));

vi.mock("../../lib/cursor-registry", () => ({
  onEditorRegistered: vi.fn(),
  onEditorUnregistered: vi.fn(),
}));

vi.mock("../../lib/editor-registry", () => ({
  registerCellEditor: vi.fn(),
  unregisterCellEditor: vi.fn(),
}));

vi.mock("../../lib/logger", () => ({
  logger: { error: vi.fn() },
}));

vi.mock("../../lib/markdown-assets", () => ({
  rewriteMarkdownAssetRefs: (source: string) => source,
}));

vi.mock("../../lib/open-url", () => ({
  openUrl: vi.fn(),
}));

vi.mock("../../lib/presence-sender", () => ({
  presenceSenderExtension: () => [],
}));

import { MarkdownCell } from "../MarkdownCell";

function makeCell(): MarkdownCellType {
  return {
    cell_type: "markdown",
    id: "md-1",
    source: "```python\nprint('hello')\n```",
    metadata: {},
  };
}

describe("MarkdownCell theme sync", () => {
  beforeEach(() => {
    mockDarkMode = false;
    mockColorTheme = undefined;
    mockIsFocused = false;
    isolatedFrameProps.length = 0;
    mockFrameHandle.send.mockClear();
    mockFrameHandle.render.mockClear();
    mockFrameHandle.renderBatch.mockClear();
    mockFrameHandle.eval.mockClear();
    mockFrameHandle.installRenderer.mockClear();
    mockFrameHandle.setTheme.mockClear();
    mockFrameHandle.clear.mockClear();
    mockFrameHandle.search.mockClear();
    mockFrameHandle.searchNavigate.mockClear();
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it("passes the current color theme to the markdown iframe and re-syncs it on ready", async () => {
    mockColorTheme = "cream";

    render(<MarkdownCell cell={makeCell()} onFocus={() => {}} onDelete={() => {}} />);

    await waitFor(() => {
      expect(mockFrameHandle.setTheme).toHaveBeenCalledWith(false, "cream");
    });

    expect(isolatedFrameProps.at(-1)?.colorTheme).toBe("cream");

    await waitFor(() => {
      expect(mockFrameHandle.render).toHaveBeenCalledWith({
        mimeType: "text/markdown",
        data: "```python\nprint('hello')\n```",
        cellId: "md-1",
        replace: true,
      });
    });
  });

  it("focuses the markdown preview without scrolling when the cell becomes focused", async () => {
    const focusSpy = vi.spyOn(HTMLElement.prototype, "focus").mockImplementation(() => undefined);

    const { rerender } = render(
      <MarkdownCell cell={makeCell()} onFocus={() => {}} onDelete={() => {}} />,
    );

    expect(focusSpy).not.toHaveBeenCalledWith({ preventScroll: true });

    mockIsFocused = true;
    rerender(<MarkdownCell cell={makeCell()} onFocus={() => {}} onDelete={() => {}} />);

    await waitFor(() => {
      expect(focusSpy).toHaveBeenCalledWith({ preventScroll: true });
    });
  });
});
