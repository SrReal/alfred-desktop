import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { request } from "../src/api";
import { DownloadPostActions } from "../src/ui/DownloadPostActions";
vi.mock("../src/api", () => ({
  request: vi.fn(),
  on: vi.fn(async () => () => {}),
}));
const call = vi.mocked(request);
const row = {
  id: "owned-id",
  name: "Owned preset",
  enabled: true,
  fingerprint: "a".repeat(64),
  registered_fingerprint: "a".repeat(64),
  definition: {
    steps: [
      {
        tool: "downloads.hash",
        input: { path: { $parameter: "download_path" } },
      },
    ],
  },
};
beforeEach(() => call.mockReset());
describe("Manual download post-actions", () => {
  it("lists a preset without running it and sends the exact reviewed observation on explicit run", async () => {
    call.mockResolvedValueOnce([row]);
    render(<DownloadPostActions />);
    await screen.findByText("Owned preset");
    expect(call).toHaveBeenCalledTimes(1);
    fireEvent.change(screen.getByLabelText("Preset de descarga"), {
      target: { value: row.id },
    });
    call.mockResolvedValueOnce(["D:\\Owned\\finished.txt"]);
    fireEvent.click(screen.getByLabelText("Archivo descargado"));
    await waitFor(() =>
      expect(screen.getByLabelText("Archivo descargado")).toHaveAttribute(
        "title",
        "D:\\Owned\\finished.txt",
      ),
    );
    expect(call).toHaveBeenCalledTimes(2);
    call.mockResolvedValueOnce({ complete: true, auto_execute: false });
    fireEvent.click(screen.getByText("Revisar acciones de descarga"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("tools.execute", {
        tool: "downloads.post_actions_run",
        input: {
          id: row.id,
          path: "D:\\Owned\\finished.txt",
          expected: { fingerprint: row.fingerprint },
        },
      }),
    );
    expect(
      await screen.findByLabelText("Resultado de acciones de descarga"),
    ).toHaveTextContent('"auto_execute": false');
  });
  it("saving a preset does not run it and malformed steps never dispatch", async () => {
    call
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce({ id: row.id, auto_execute: false })
      .mockResolvedValueOnce([row]);
    render(<DownloadPostActions />);
    await waitFor(() => expect(call).toHaveBeenCalledTimes(1));
    fireEvent.change(screen.getByLabelText("Nombre del preset"), {
      target: { value: "Owned new" },
    });
    fireEvent.change(screen.getByLabelText("Pasos del preset"), {
      target: { value: "{}" },
    });
    fireEvent.click(screen.getByText("Revisar y guardar preset"));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "uno a diez pasos",
    );
    expect(call).toHaveBeenCalledTimes(1);
    fireEvent.change(screen.getByLabelText("Pasos del preset"), {
      target: { value: '[{"tool":"downloads.hash","input":{}}]' },
    });
    fireEvent.click(screen.getByText("Revisar y guardar preset"));
    await screen.findByText("Owned preset");
    expect(call).toHaveBeenCalledWith("tools.execute", {
      tool: "downloads.post_actions_save",
      input: {
        name: "Owned new",
        steps: [{ tool: "downloads.hash", input: {} }],
      },
    });
    expect(
      call.mock.calls.some(
        (c) =>
          typeof c[1] === "object" &&
          c[1] !== null &&
          !Array.isArray(c[1]) &&
          c[1].tool === "downloads.post_actions_run",
      ),
    ).toBe(false);
  });
  it("blocks a changed preset and displays one error without replay", async () => {
    call.mockResolvedValueOnce([
      { ...row, registered_fingerprint: "b".repeat(64) },
    ]);
    render(<DownloadPostActions />);
    await screen.findByText("Owned preset");
    fireEvent.change(screen.getByLabelText("Preset de descarga"), {
      target: { value: row.id },
    });
    call.mockResolvedValueOnce(["D:\\Owned\\finished.txt"]);
    fireEvent.click(screen.getByLabelText("Archivo descargado"));
    await waitFor(() =>
      expect(screen.getByLabelText("Archivo descargado")).toHaveAttribute(
        "title",
        "D:\\Owned\\finished.txt",
      ),
    );
    expect(screen.getByText("Revisar acciones de descarga")).toBeDisabled();
    expect(call).toHaveBeenCalledTimes(2);
    fireEvent.change(screen.getByLabelText("Nombre del preset"), {
      target: { value: "Denied preset" },
    });
    call.mockRejectedValueOnce(new Error("PERMISSION_DENIED_OR_TIMEOUT"));
    fireEvent.click(screen.getByText("Revisar y guardar preset"));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "PERMISSION_DENIED_OR_TIMEOUT",
    );
    expect(call).toHaveBeenCalledTimes(3);
  });
});
