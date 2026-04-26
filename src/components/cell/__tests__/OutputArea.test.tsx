import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { OutputArea, type JupyterOutput } from "../OutputArea";

let mockDarkMode = false;
let mockColorTheme: string | undefined;

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
  isReady: false,
  isIframeReady: true,
};

vi.mock("@/lib/dark-mode", () => ({
  useDarkMode: () => mockDarkMode,
  useColorTheme: () => mockColorTheme,
}));

vi.mock("@/components/isolated/iframe-libraries", () => ({
  injectPluginsForMimes: vi.fn(async () => {}),
  needsPlugin: vi.fn(() => false),
}));

vi.mock("@/components/isolated", async () => {
  const React = await import("react");

  const MockIsolatedFrame = React.forwardRef<
    typeof mockFrameHandle,
    { allowWheelBoundaryScroll?: boolean; className?: string; onReady?: () => void }
  >(function MockIsolatedFrame({ allowWheelBoundaryScroll, className, onReady }, ref) {
    React.useImperativeHandle(ref, () => mockFrameHandle);

    React.useEffect(() => {
      onReady?.();
    }, [onReady]);

    return (
      <div
        className={className}
        data-allow-wheel-boundary-scroll={String(allowWheelBoundaryScroll)}
        data-testid="isolated-frame"
      />
    );
  });

  return {
    CommBridgeManager: class CommBridgeManager {},
    IsolatedFrame: MockIsolatedFrame,
  };
});

function makeMarkdownOutput(content = "```python\nprint('hello')\n```"): JupyterOutput[] {
  return [
    {
      output_type: "display_data",
      data: { "text/markdown": content },
      metadata: {},
    },
  ];
}

function makeLargeStreamOutput(): JupyterOutput[] {
  return [
    {
      output_type: "stream",
      name: "stdout",
      text: Array.from({ length: 160 }, (_, index) => `log line ${index}`).join("\n"),
    },
  ];
}

describe("OutputArea output well", () => {
  beforeEach(() => {
    mockDarkMode = false;
    mockColorTheme = undefined;
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

  it("re-sends the current cream color theme when the iframe becomes ready", async () => {
    mockColorTheme = "cream";

    render(<OutputArea outputs={makeMarkdownOutput()} isolated />);

    await waitFor(() => {
      expect(mockFrameHandle.setTheme).toHaveBeenCalledWith(false, "cream");
    });

    await waitFor(() => {
      expect(mockFrameHandle.renderBatch).toHaveBeenCalledWith([
        expect.objectContaining({
          mimeType: "text/markdown",
          data: "```python\nprint('hello')\n```",
        }),
      ]);
    });
  });

  it("sends null for classic so a reloaded iframe clears stale cream state", async () => {
    mockColorTheme = undefined;

    render(<OutputArea outputs={makeMarkdownOutput()} isolated />);

    await waitFor(() => {
      expect(mockFrameHandle.setTheme).toHaveBeenCalledWith(false, null);
    });
  });

  it("keeps isolated output iframes passive until the output well is activated", async () => {
    const onIframeMouseDown = vi.fn();

    render(
      <OutputArea outputs={makeMarkdownOutput()} isolated onIframeMouseDown={onIframeMouseDown} />,
    );

    const frame = screen.getByTestId("isolated-frame");
    expect(frame.getAttribute("class") ?? "").toContain("pointer-events-none");
    expect(frame.getAttribute("data-allow-wheel-boundary-scroll")).toBe("false");

    const activator = screen.getByRole("button", { name: "Activate output well" });
    fireEvent.click(activator);

    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Activate output well" })).toBeNull();
    });
    expect(frame.getAttribute("class") ?? "").not.toContain("pointer-events-none");
    expect(onIframeMouseDown).toHaveBeenCalled();
  });

  it("bounds large in-DOM stream outputs behind the same output well", async () => {
    const onOutputWellInteractiveChange = vi.fn();
    const { container } = render(
      <OutputArea
        outputs={makeLargeStreamOutput()}
        outputWellInteractive={false}
        onOutputWellInteractiveChange={onOutputWellInteractiveChange}
      />,
    );

    const outputWell = container.querySelector('[data-slot="output-well"]');
    expect(outputWell?.getAttribute("data-interactive")).toBe("false");
    expect(container.querySelector('[data-slot="isolated-frame"]')).toBeNull();
    expect(container.querySelector(".max-h-\\[420px\\]")).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Activate output well" }));

    expect(onOutputWellInteractiveChange).toHaveBeenCalledWith(true);
  });
});
