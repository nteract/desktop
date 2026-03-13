import { render, screen } from "@testing-library/react";
import { CondaDependencyHeader } from "../CondaDependencyHeader";
import { DependencyHeader } from "../DependencyHeader";

describe("dependency panel copy", () => {
  it("uses re-initialize language for uv dependency changes", () => {
    render(
      <DependencyHeader
        dependencies={[]}
        requiresPython={null}
        uvAvailable={true}
        loading={false}
        syncedWhileRunning
        needsKernelRestart
        onAdd={async () => {}}
        onRemove={async () => {}}
        syncState={{
          status: "dirty",
          added: ["pandas"],
          removed: [],
        }}
        onSyncNow={async () => true}
      />,
    );

    expect(
      screen.getByText(/re-initialize the environment to use these dependencies/i),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/re-initialize the environment if you updated existing packages/i),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /re-initialize/i }),
    ).toBeInTheDocument();
  });

  it("uses re-initialize language for conda dependency changes", () => {
    render(
      <CondaDependencyHeader
        dependencies={[]}
        channels={[]}
        python={null}
        loading={false}
        syncing={false}
        syncState={{
          status: "dirty",
        }}
        syncedWhileRunning={false}
        needsKernelRestart={false}
        onAdd={async () => {}}
        onRemove={async () => {}}
        onSetChannels={async () => {}}
        onSetPython={async () => {}}
        onSyncNow={async () => true}
      />,
    );

    expect(
      screen.getByText(/dependencies changed — re-initialize environment to apply/i),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /re-initialize/i }),
    ).toBeInTheDocument();
  });
});
