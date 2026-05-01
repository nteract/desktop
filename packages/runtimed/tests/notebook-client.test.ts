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
    });

    expect(sendRequest).toHaveBeenCalledWith({
      type: "sync_environment",
      guard: {
        observed_heads: ["head-a", "head-b"],
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

  it("clones a notebook as an ephemeral room", async () => {
    const { client, sendRequest } = stubClient();
    sendRequest.mockResolvedValueOnce({
      result: "notebook_cloned",
      notebook_id: "clone-1",
      working_dir: "/tmp/project",
    });

    await expect(client.cloneAsEphemeral("source-1")).resolves.toEqual({
      notebookId: "clone-1",
      workingDir: "/tmp/project",
    });
    expect(sendRequest).toHaveBeenCalledWith({
      type: "clone_as_ephemeral",
      source_notebook_id: "source-1",
    });
  });
});
