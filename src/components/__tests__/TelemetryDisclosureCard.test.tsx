import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { TelemetryDisclosureCard } from "../TelemetryDisclosureCard";

describe("TelemetryDisclosureCard", () => {
  it("renders the eyebrow, body, and a Learn more link", () => {
    render(<TelemetryDisclosureCard />);
    expect(screen.getByText(/One anonymous daily ping/i)).toBeInTheDocument();
    expect(
      screen.getByText(/Version, platform, architecture/i),
    ).toBeInTheDocument();
    const link = screen.getByRole("link", { name: /read the full details/i });
    expect(link).toHaveAttribute("href", "https://nteract.io/telemetry");
  });

  it("renders a footer slot when provided", () => {
    render(<TelemetryDisclosureCard footer={<span>extra</span>} />);
    expect(screen.getByText("extra")).toBeInTheDocument();
  });
});
