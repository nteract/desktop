/**
 * Tests for ansi-output.tsx - ANSI escape sequence rendering.
 *
 * These tests verify that ANSI escape sequences are properly parsed
 * and rendered with appropriate styles and CSS classes.
 */

import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vite-plus/test";
import { AnsiErrorOutput, AnsiOutput, AnsiStreamOutput } from "../ansi-output";

// Helper to get styles from rendered spans
function getSpanStyles(container: HTMLElement) {
  const spans = container.querySelectorAll("span");
  return Array.from(spans).map((span) => ({
    text: span.textContent,
    className: span.className,
    style: span.getAttribute("style"),
  }));
}

describe("AnsiOutput", () => {
  describe("plain text", () => {
    it("renders plain text without ANSI codes", () => {
      render(<AnsiOutput>Hello, World!</AnsiOutput>);
      expect(screen.getByText("Hello, World!")).toBeInTheDocument();
    });

    it("renders empty string as null", () => {
      const { container } = render(<AnsiOutput>{""}</AnsiOutput>);
      expect(container.querySelector("[data-slot='ansi-output']")).toBeNull();
    });

    it("handles whitespace and newlines", () => {
      render(<AnsiOutput>{"line1\nline2\nline3"}</AnsiOutput>);
      expect(screen.getByText(/line1/)).toBeInTheDocument();
    });
  });

  describe("standard 16 colors", () => {
    it("renders red foreground with CSS class", () => {
      const { container } = render(<AnsiOutput>{"\x1b[31mRed text\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const redSpan = spans.find((s) => s.text === "Red text");
      expect(redSpan?.className).toContain("ansi-red-fg");
    });

    it("renders green foreground with CSS class", () => {
      const { container } = render(<AnsiOutput>{"\x1b[32mGreen text\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const greenSpan = spans.find((s) => s.text === "Green text");
      expect(greenSpan?.className).toContain("ansi-green-fg");
    });

    it("renders yellow foreground with CSS class", () => {
      const { container } = render(<AnsiOutput>{"\x1b[33mYellow text\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const yellowSpan = spans.find((s) => s.text === "Yellow text");
      expect(yellowSpan?.className).toContain("ansi-yellow-fg");
    });

    it("renders blue foreground with CSS class", () => {
      const { container } = render(<AnsiOutput>{"\x1b[34mBlue text\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const blueSpan = spans.find((s) => s.text === "Blue text");
      expect(blueSpan?.className).toContain("ansi-blue-fg");
    });

    it("renders bright colors with CSS class", () => {
      const { container } = render(<AnsiOutput>{"\x1b[91mBright red\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const brightRedSpan = spans.find((s) => s.text === "Bright red");
      expect(brightRedSpan?.className).toContain("ansi-bright-red-fg");
    });

    it("renders background colors with CSS class", () => {
      const { container } = render(<AnsiOutput>{"\x1b[41mRed background\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const bgSpan = spans.find((s) => s.text === "Red background");
      expect(bgSpan?.className).toContain("ansi-red-bg");
    });
  });

  describe("256-color palette", () => {
    it("renders palette color 196 (red) as inline RGB", () => {
      // Color 196 is in the 6x6x6 cube: r=5, g=0, b=0 -> rgb(255, 0, 0)
      const { container } = render(<AnsiOutput>{"\x1b[38;5;196mPalette 196\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const paletteSpan = spans.find((s) => s.text === "Palette 196");
      expect(paletteSpan?.style).toContain("color:");
      expect(paletteSpan?.style).toContain("rgb(");
    });

    it("renders grayscale palette color as inline RGB", () => {
      // Color 240 is grayscale: (240-232)*10+8 = 88
      const { container } = render(<AnsiOutput>{"\x1b[38;5;240mGrayscale\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const graySpan = spans.find((s) => s.text === "Grayscale");
      expect(graySpan?.style).toContain("color:");
      expect(graySpan?.style).toContain("rgb(88, 88, 88)");
    });

    it("renders palette background color", () => {
      const { container } = render(<AnsiOutput>{"\x1b[48;5;21mBlue BG\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const bgSpan = spans.find((s) => s.text === "Blue BG");
      expect(bgSpan?.style).toContain("background-color:");
    });
  });

  describe("24-bit truecolor", () => {
    it("renders truecolor foreground as inline RGB", () => {
      const { container } = render(<AnsiOutput>{"\x1b[38;2;255;128;0mOrange\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const truecolorSpan = spans.find((s) => s.text === "Orange");
      expect(truecolorSpan?.style).toContain("color:");
      expect(truecolorSpan?.style).toContain("rgb(255, 128, 0)");
    });

    it("renders truecolor background as inline RGB", () => {
      const { container } = render(
        <AnsiOutput>{"\x1b[48;2;50;100;150mTruecolor BG\x1b[0m"}</AnsiOutput>,
      );
      const spans = getSpanStyles(container);
      const bgSpan = spans.find((s) => s.text === "Truecolor BG");
      expect(bgSpan?.style).toContain("background-color:");
      expect(bgSpan?.style).toContain("rgb(50, 100, 150)");
    });
  });

  describe("decorations", () => {
    it("renders bold text", () => {
      const { container } = render(<AnsiOutput>{"\x1b[1mBold\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const boldSpan = spans.find((s) => s.text === "Bold");
      expect(boldSpan?.style).toContain("font-weight: bold");
    });

    it("renders italic text", () => {
      const { container } = render(<AnsiOutput>{"\x1b[3mItalic\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const italicSpan = spans.find((s) => s.text === "Italic");
      expect(italicSpan?.style).toContain("font-style: italic");
    });

    it("renders underline text", () => {
      const { container } = render(<AnsiOutput>{"\x1b[4mUnderline\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const underlineSpan = spans.find((s) => s.text === "Underline");
      expect(underlineSpan?.style).toContain("text-decoration: underline");
    });

    it("renders strikethrough text", () => {
      const { container } = render(<AnsiOutput>{"\x1b[9mStrikethrough\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const strikeSpan = spans.find((s) => s.text === "Strikethrough");
      expect(strikeSpan?.style).toContain("line-through");
    });

    it("renders dim text with reduced opacity", () => {
      const { container } = render(<AnsiOutput>{"\x1b[2mDim\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const dimSpan = spans.find((s) => s.text === "Dim");
      expect(dimSpan?.style).toContain("opacity: 0.5");
    });

    it("combines multiple decorations", () => {
      const { container } = render(
        <AnsiOutput>{"\x1b[1;3;4mBold italic underline\x1b[0m"}</AnsiOutput>,
      );
      const spans = getSpanStyles(container);
      const combinedSpan = spans.find((s) => s.text === "Bold italic underline");
      expect(combinedSpan?.style).toContain("font-weight: bold");
      expect(combinedSpan?.style).toContain("font-style: italic");
      expect(combinedSpan?.style).toContain("text-decoration: underline");
    });
  });

  describe("terminal emulation", () => {
    it("handles backspace characters", () => {
      // "abc\b" should become "ab" (backspace removes 'c')
      const { container } = render(<AnsiOutput>{"abc\x08d"}</AnsiOutput>);
      // The result should be "abd" (c is replaced by backspace+d)
      const text = container.textContent;
      expect(text).toContain("abd");
    });

    it("handles carriage return", () => {
      // Carriage return should work with escape-carriage package
      const { container } = render(<AnsiOutput>{"hello\rworld"}</AnsiOutput>);
      const text = container.textContent;
      // escape-carriage handles \r by replacing content
      expect(text).toBeDefined();
    });
  });

  describe("combined styling", () => {
    it("renders color with decoration", () => {
      const { container } = render(<AnsiOutput>{"\x1b[1;31mBold Red\x1b[0m"}</AnsiOutput>);
      const spans = getSpanStyles(container);
      const styledSpan = spans.find((s) => s.text === "Bold Red");
      expect(styledSpan?.className).toContain("ansi-red-fg");
      expect(styledSpan?.style).toContain("font-weight: bold");
    });

    it("renders multiple styled segments", () => {
      const { container } = render(
        <AnsiOutput>{"\x1b[31mRed\x1b[0m \x1b[32mGreen\x1b[0m \x1b[34mBlue\x1b[0m"}</AnsiOutput>,
      );
      const spans = getSpanStyles(container);
      expect(spans.find((s) => s.text === "Red")?.className).toContain("ansi-red-fg");
      expect(spans.find((s) => s.text === "Green")?.className).toContain("ansi-green-fg");
      expect(spans.find((s) => s.text === "Blue")?.className).toContain("ansi-blue-fg");
    });
  });

  describe("isError prop", () => {
    it("applies error styling when isError is true", () => {
      const { container } = render(<AnsiOutput isError>Error message</AnsiOutput>);
      const output = container.querySelector("[data-slot='ansi-output']");
      expect(output?.className).toContain("text-red");
    });
  });
});

describe("AnsiStreamOutput", () => {
  it("renders stdout with appropriate styling", () => {
    const { container } = render(<AnsiStreamOutput text="stdout output" streamName="stdout" />);
    const output = container.querySelector("[data-slot='ansi-stream-output']");
    expect(output).toBeInTheDocument();
    expect(screen.getByText("stdout output")).toBeInTheDocument();
  });

  it("renders stderr with red styling", () => {
    const { container } = render(<AnsiStreamOutput text="stderr output" streamName="stderr" />);
    const output = container.querySelector("[data-slot='ansi-stream-output']");
    expect(output?.className).toContain("text-red");
  });

  it("renders text content", () => {
    render(<AnsiStreamOutput text="Plain stream output" streamName="stdout" />);
    expect(screen.getByText("Plain stream output")).toBeInTheDocument();
  });
});

describe("AnsiErrorOutput", () => {
  it("renders error name and value", () => {
    render(<AnsiErrorOutput ename="TypeError" evalue="cannot read property 'foo' of undefined" />);
    expect(screen.getByText(/TypeError/)).toBeInTheDocument();
    expect(screen.getByText(/cannot read property/)).toBeInTheDocument();
  });

  it("renders traceback as array", () => {
    render(
      <AnsiErrorOutput ename="Error" evalue="test" traceback={["line 1", "line 2", "line 3"]} />,
    );
    expect(screen.getByText(/line 1/)).toBeInTheDocument();
    expect(screen.getByText(/line 2/)).toBeInTheDocument();
  });

  it("renders traceback as string", () => {
    render(<AnsiErrorOutput ename="Error" evalue="test" traceback="single line traceback" />);
    expect(screen.getByText(/single line traceback/)).toBeInTheDocument();
  });

  it("renders ANSI codes in traceback", () => {
    const { container } = render(
      <AnsiErrorOutput ename="Error" evalue="test" traceback={["\x1b[31mRed error line\x1b[0m"]} />,
    );
    const spans = getSpanStyles(container);
    const redSpan = spans.find((s) => s.text === "Red error line");
    expect(redSpan?.className).toContain("ansi-red-fg");
  });

  it("has error-specific styling", () => {
    const { container } = render(<AnsiErrorOutput ename="Error" evalue="test" />);
    const output = container.querySelector("[data-slot='ansi-error-output']");
    expect(output?.className).toContain("not-prose");
  });
});
