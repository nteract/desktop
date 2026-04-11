/**
 * Notebook demo — multiple SiftTable instances on one page.
 * Tests performance, WASM module sharing, and memory with N tables.
 */
import { createRoot } from "react-dom/client";
import { SiftTable } from "./react";
import "./style.css";

const cells = [
  { label: "Generated Dataset (100k rows)", url: "/data.arrow" },
  { label: "Generated Dataset (100k rows)", url: "/data.arrow" },
  { label: "Generated Dataset (100k rows)", url: "/data.arrow" },
  { label: "Generated Dataset (100k rows)", url: "/data.arrow" },
  { label: "Generated Dataset (100k rows)", url: "/data.arrow" },
];

function NotebookDemo() {
  return (
    <div
      style={{
        maxWidth: 1200,
        margin: "0 auto",
        padding: "24px 16px",
        fontFamily: "Inter, system-ui, sans-serif",
      }}
    >
      <div style={{ marginBottom: 24 }}>
        <h1
          style={{
            fontSize: 24,
            fontWeight: 700,
            margin: "0 0 4px",
            color: "#2d2016",
          }}
        >
          Notebook Demo
        </h1>
        <p style={{ color: "#8b7355", margin: 0, fontSize: 14 }}>
          {cells.length} tables on one page — testing multi-instance performance
        </p>
      </div>

      {cells.map((cell, i) => (
        <div
          key={i}
          style={{
            marginBottom: 32,
            border: "1px solid #e8ddd0",
            borderRadius: 12,
            overflow: "hidden",
          }}
        >
          <div
            style={{
              padding: "8px 16px",
              background: "#f5f0e8",
              borderBottom: "1px solid #e8ddd0",
              fontSize: 13,
              color: "#8b7355",
              fontFamily: "monospace",
            }}
          >
            [{i + 1}] {cell.label}
          </div>
          <div style={{ height: 400 }}>
            <SiftTable url={cell.url} />
          </div>
        </div>
      ))}
    </div>
  );
}

const root = document.getElementById("app")!;
createRoot(root).render(<NotebookDemo />);
