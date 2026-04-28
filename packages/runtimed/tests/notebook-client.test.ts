import { NotebookClient, type NotebookTransport } from "runtimed";
import { describe, expect, it, vi } from "vite-plus/test";

function stubClient() {
  const sendRequest = vi.fn().mockResolvedValue({ result: "sync_environment_complete" });
  const transport = {
    sendFrame: async () => {},
    onFrame: () => () => {},
    sendRequest,
    connected: true,
    disconnect: () => {},
  } satisfies NotebookTransport;

  return { client: new NotebookClient({ transport }), sendRequest };
}

describe("NotebookClient", () => {
  it("emits unguarded sync_environment requests", async () => {
    const { client, sendRequest } = stubClient();

    await client.syncEnvironment();

    expect(sendRequest).toHaveBeenCalledWith({ type: "sync_environment" });
  });

  it("emits guarded sync_environment requests with dependency provenance", async () => {
    const { client, sendRequest } = stubClient();

    await client.syncEnvironment({
      observed_heads: ["head-a", "head-b"],
      dependency_fingerprint: '{"uv":{"dependencies":["numpy"]}}',
    });

    expect(sendRequest).toHaveBeenCalledWith({
      type: "sync_environment",
      guard: {
        observed_heads: ["head-a", "head-b"],
        dependency_fingerprint: '{"uv":{"dependencies":["numpy"]}}',
      },
    });
  });

  it("emits project environment approval requests", async () => {
    const { client, sendRequest } = stubClient();

    await client.approveProjectEnvironment("/tmp/project/environment.yml");

    expect(sendRequest).toHaveBeenCalledWith({
      type: "approve_project_environment",
      project_file_path: "/tmp/project/environment.yml",
    });
  });
});
