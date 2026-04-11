import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { ErrorBoundary } from "../error-boundary";

// Suppress React's noisy error boundary console output during tests
beforeEach(() => {
  vi.spyOn(console, "error").mockImplementation(() => {});
});

function ThrowingChild({ shouldThrow }: { shouldThrow: boolean }) {
  if (shouldThrow) {
    throw new Error("render failure");
  }
  return <div>child content</div>;
}

describe("ErrorBoundary", () => {
  it("renders children when no error occurs", () => {
    render(
      <ErrorBoundary fallback={(error) => <div>{error.message}</div>}>
        <div>hello</div>
      </ErrorBoundary>,
    );
    expect(screen.getByText("hello")).toBeInTheDocument();
  });

  it("renders fallback when a child throws", () => {
    render(
      <ErrorBoundary fallback={(error) => <div>error: {error.message}</div>}>
        <ThrowingChild shouldThrow />
      </ErrorBoundary>,
    );
    expect(screen.getByText("error: render failure")).toBeInTheDocument();
  });

  it("calls onError when a child throws", () => {
    const onError = vi.fn();
    render(
      <ErrorBoundary
        onError={onError}
        fallback={(error) => <div>{error.message}</div>}
      >
        <ThrowingChild shouldThrow />
      </ErrorBoundary>,
    );
    expect(onError).toHaveBeenCalledOnce();
    expect(onError.mock.calls[0][0].message).toBe("render failure");
  });

  it("recovers when resetErrorBoundary is called", () => {
    function Wrapper() {
      const [shouldThrow, setShouldThrow] = useState(true);
      return (
        <ErrorBoundary
          fallback={(_error, reset) => (
            <button
              onClick={() => {
                setShouldThrow(false);
                reset();
              }}
            >
              retry
            </button>
          )}
        >
          <ThrowingChild shouldThrow={shouldThrow} />
        </ErrorBoundary>
      );
    }

    render(<Wrapper />);
    expect(screen.getByText("retry")).toBeInTheDocument();

    fireEvent.click(screen.getByText("retry"));
    expect(screen.getByText("child content")).toBeInTheDocument();
  });

  describe("resetKeys", () => {
    it("auto-resets error state when resetKeys change", () => {
      function Wrapper() {
        const [key, setKey] = useState(0);
        return (
          <>
            <button onClick={() => setKey((k) => k + 1)}>bump</button>
            <ErrorBoundary
              resetKeys={[key]}
              fallback={(error) => <div>error: {error.message}</div>}
            >
              {/* Only throws on initial render (key=0) */}
              <ThrowingChild shouldThrow={key === 0} />
            </ErrorBoundary>
          </>
        );
      }

      render(<Wrapper />);
      // Initially in error state
      expect(screen.getByText("error: render failure")).toBeInTheDocument();

      // Bump resetKeys — error should clear and children re-render
      fireEvent.click(screen.getByText("bump"));
      expect(screen.getByText("child content")).toBeInTheDocument();
    });

    it("stays in error state when resetKeys do not change", () => {
      function Wrapper() {
        const [, setCounter] = useState(0);
        return (
          <>
            <button onClick={() => setCounter((c) => c + 1)}>rerender</button>
            <ErrorBoundary
              resetKeys={["stable"]}
              fallback={(error) => <div>error: {error.message}</div>}
            >
              <ThrowingChild shouldThrow />
            </ErrorBoundary>
          </>
        );
      }

      render(<Wrapper />);
      expect(screen.getByText("error: render failure")).toBeInTheDocument();

      // Parent re-render without changing resetKeys — still in error
      fireEvent.click(screen.getByText("rerender"));
      expect(screen.getByText("error: render failure")).toBeInTheDocument();
    });

    it("resets when output object reference changes", () => {
      function Wrapper() {
        const [output, setOutput] = useState<{ data: string }>({ data: "bad" });
        return (
          <>
            <button onClick={() => setOutput({ data: "good" })}>
              new-output
            </button>
            <ErrorBoundary
              resetKeys={[output]}
              fallback={(error) => <div>error: {error.message}</div>}
            >
              <ThrowingChild shouldThrow={output.data === "bad"} />
            </ErrorBoundary>
          </>
        );
      }

      render(<Wrapper />);
      expect(screen.getByText("error: render failure")).toBeInTheDocument();

      // New output object — boundary should reset
      fireEvent.click(screen.getByText("new-output"));
      expect(screen.getByText("child content")).toBeInTheDocument();
    });
  });
});
