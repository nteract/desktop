import { IsolatedFrame, type IsolatedFrameHandle } from "@/components/isolated/isolated-frame";
import { IsolatedRendererProvider } from "@/components/isolated/isolated-renderer-context";
import { injectPluginsForMimes, needsPlugin } from "@/components/isolated/iframe-libraries";
import { createRoot } from "react-dom/client";
import { useCallback, useRef, useState } from "react";
import { fixtures, type Fixture } from "./fixtures";

function FixtureCard({ fixture, index }: { fixture: Fixture; index: number }) {
  const frameRef = useRef<IsolatedFrameHandle>(null);
  const injectedRef = useRef(new Set<string>());
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const onReady = useCallback(async () => {
    const frame = frameRef.current;
    if (!frame) return;

    // Install plugin if this MIME type needs one
    if (needsPlugin(fixture.mimeType)) {
      await injectPluginsForMimes(frame, [fixture.mimeType], injectedRef.current);
    }

    // Send the render message
    frame.render({
      mimeType: fixture.mimeType,
      data: fixture.data,
      cellId: `fixture-${index}`,
      outputIndex: 0,
    });

    setReady(true);
  }, [fixture, index]);

  return (
    <div
      style={{
        border: "1px solid #e0e0e0",
        borderRadius: 8,
        overflow: "hidden",
        marginBottom: 16,
      }}
    >
      <div
        style={{
          padding: "8px 12px",
          background: "#f5f5f5",
          borderBottom: "1px solid #e0e0e0",
          display: "flex",
          alignItems: "center",
          gap: 8,
          fontFamily: "system-ui, sans-serif",
          fontSize: 13,
        }}
      >
        <span
          data-testid={`fixture-status-${index}`}
          data-ready={ready}
          style={{
            width: 8,
            height: 8,
            borderRadius: "50%",
            background: error ? "#ef4444" : ready ? "#22c55e" : "#d4d4d4",
          }}
        />
        <strong>{fixture.label}</strong>
        <code style={{ color: "#6b7280", fontSize: 11 }}>{fixture.mimeType}</code>
        {error && <span style={{ color: "#ef4444", fontSize: 11 }}>{error}</span>}
      </div>
      <div data-testid={`fixture-frame-${index}`}>
        <IsolatedFrame
          ref={frameRef}
          id={`fixture-${index}`}
          onReady={onReady}
          onError={(e) => setError(e.message)}
          minHeight={40}
          maxHeight={400}
        />
      </div>
    </div>
  );
}

function App() {
  return (
    <IsolatedRendererProvider loader={() => import("virtual:isolated-renderer")}>
      <div
        style={{
          maxWidth: 900,
          margin: "0 auto",
          padding: "24px 16px",
          fontFamily: "system-ui, sans-serif",
        }}
      >
        <h1 style={{ fontSize: 20, fontWeight: 700, marginBottom: 4 }}>Renderer Plugin Test</h1>
        <p style={{ color: "#6b7280", fontSize: 13, marginBottom: 24 }}>
          {fixtures.length} fixtures — each rendered in an isolated iframe
        </p>
        {fixtures.map((fixture, i) => (
          <FixtureCard key={i} fixture={fixture} index={i} />
        ))}
      </div>
    </IsolatedRendererProvider>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
