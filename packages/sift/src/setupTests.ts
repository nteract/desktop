import "@testing-library/jest-dom/vitest";
import { vi } from "vitest";

// --- Canvas 2D context stubs (jsdom doesn't implement canvas) ---

const canvasStub = {
  getContext: () => ({
    setTransform: vi.fn(),
    clearRect: vi.fn(),
    fillRect: vi.fn(),
    strokeRect: vi.fn(),
    fillText: vi.fn(),
    strokeText: vi.fn(),
    measureText: vi.fn((text: string) => ({ width: text.length * 7 })),
    beginPath: vi.fn(),
    closePath: vi.fn(),
    moveTo: vi.fn(),
    lineTo: vi.fn(),
    arc: vi.fn(),
    arcTo: vi.fn(),
    quadraticCurveTo: vi.fn(),
    bezierCurveTo: vi.fn(),
    rect: vi.fn(),
    fill: vi.fn(),
    stroke: vi.fn(),
    clip: vi.fn(),
    save: vi.fn(),
    restore: vi.fn(),
    scale: vi.fn(),
    rotate: vi.fn(),
    translate: vi.fn(),
    transform: vi.fn(),
    createLinearGradient: vi.fn(() => ({ addColorStop: vi.fn() })),
    createRadialGradient: vi.fn(() => ({ addColorStop: vi.fn() })),
    createPattern: vi.fn(),
    drawImage: vi.fn(),
    getImageData: vi.fn(() => ({ data: new Uint8ClampedArray(0) })),
    putImageData: vi.fn(),
    canvas: { width: 300, height: 150 },
    font: "",
    textAlign: "start" as CanvasTextAlign,
    textBaseline: "alphabetic" as CanvasTextBaseline,
    globalAlpha: 1,
    globalCompositeOperation: "source-over" as GlobalCompositeOperation,
    lineWidth: 1,
    lineCap: "butt" as CanvasLineCap,
    lineJoin: "miter" as CanvasLineJoin,
    miterLimit: 10,
    shadowBlur: 0,
    shadowColor: "rgba(0, 0, 0, 0)",
    shadowOffsetX: 0,
    shadowOffsetY: 0,
    fillStyle: "#000",
    strokeStyle: "#000",
  }),
  toDataURL: vi.fn(() => ""),
  toBlob: vi.fn(),
  width: 300,
  height: 150,
};

HTMLCanvasElement.prototype.getContext = canvasStub.getContext as never;

// --- document.fonts polyfill ---

Object.defineProperty(document, "fonts", {
  value: { ready: Promise.resolve() },
  writable: false,
});

// --- Element.requestFullscreen stub ---

HTMLElement.prototype.requestFullscreen = vi.fn(() => Promise.resolve());

// --- Mock @chenglou/pretext (requires real canvas font measurement) ---

vi.mock("@chenglou/pretext", () => ({
  prepare: vi.fn(() => ({ __brand: "PreparedText" })),
  prepareWithSegments: vi.fn((text: string) => ({
    __brand: "PreparedTextWithSegments",
    widths: [text.length * 7], // rough estimate: 7px per char
    segments: [text],
  })),
  layout: vi.fn(() => ({ lineCount: 1, height: 20 })),
}));
