import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { request } from "../src/api";
import { PluginManager } from "../src/ui/PluginManager";
vi.mock("../src/api", () => ({
  request: vi.fn(),
  on: vi.fn(async () => () => {}),
}));
const call = vi.mocked(request);
const summary = {
  id: "custom.owned",
  name: "Owned",
  version: "1.0.0",
  state: "enabled",
  runtime: "node",
};
const detail = {
  ...summary,
  path: "D:\\Owned\\v1",
  fingerprint: "a".repeat(64),
  manifest: {
    name: "Owned",
    version: "1.0.0",
    description: "Owned fixture",
    sideEffects: ["Own process"],
    rollback: "None",
    inputSchema: { type: "object" },
    outputSchema: { type: "object" },
    permissions: { defaultRisk: "mandatory" },
    secrets: [],
  },
  tests: { status: "PASS" },
  isolation: "Owned local process",
};
beforeEach(() => call.mockReset());
describe("Plugin Builder review", () => {
  it("requires explicit code/side-effect review and saves a disabled draft without testing or enabling", async () => {
    call.mockResolvedValueOnce([]);
    render(<PluginManager />);
    await waitFor(() => expect(call).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByText("Cargar ejemplo de eco"));
    expect(screen.getByText("Revisar y generar borrador")).toBeDisabled();
    fireEvent.click(screen.getByLabelText(/He revisado código/));
    call
      .mockResolvedValueOnce({
        id: "custom.reviewed-echo",
        state: "draft",
        enabled: false,
      })
      .mockResolvedValueOnce([]);
    fireEvent.click(screen.getByText("Revisar y generar borrador"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith(
        "tools.execute",
        expect.objectContaining({ tool: "plugin_builder.create" }),
      ),
    );
    expect(
      call.mock.calls.filter(
        ([, v]) =>
          v &&
          typeof v === "object" &&
          "tool" in v &&
          [
            "plugin_builder.test",
            "plugin_builder.enable_plugin",
            "custom.run",
          ].includes(String(v.tool)),
      ),
    ).toHaveLength(0);
  });
  it("executes only with an inspected fingerprint and exposes failure without retry", async () => {
    call.mockResolvedValueOnce([summary]).mockResolvedValueOnce(detail);
    render(<PluginManager />);
    fireEvent.click(await screen.findByText("Inspeccionar custom.owned"));
    await screen.findByText("Estado: enabled");
    expect(call).toHaveBeenCalledTimes(2);
    fireEvent.change(screen.getByLabelText("Entrada JSON"), {
      target: { value: '{"text":"ñ-日本"}' },
    });
    call.mockRejectedValueOnce(new Error("PLUGIN_CHANGED"));
    fireEvent.click(screen.getByText("Revisar y ejecutar herramienta"));
    await screen.findByRole("alert");
    expect(call).toHaveBeenCalledWith("tools.execute", {
      tool: "custom.run",
      input: {
        id: "custom.owned",
        expected: { fingerprint: detail.fingerprint },
        body: { text: "ñ-日本" },
      },
    });
    expect(call).toHaveBeenCalledTimes(3);
  });
  it("opens an official Claude conversation for a proposal and leaves registration to explicit review", async () => {
    call.mockResolvedValueOnce([]);
    render(<PluginManager />);
    await waitFor(() => expect(call).toHaveBeenCalledTimes(1));
    fireEvent.change(screen.getByLabelText("Describe la herramienta"), {
      target: { value: "Revisar un servicio" },
    });
    call
      .mockResolvedValueOnce({ id: "conversation" })
      .mockResolvedValueOnce({ run_id: "run" })
      .mockResolvedValueOnce({})
      .mockResolvedValueOnce([]);
    fireEvent.click(screen.getByText("Pedir propuesta a Claude"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("conversations.open", {
        id: "conversation",
      }),
    );
    expect(call).toHaveBeenCalledWith(
      "chat.send",
      expect.objectContaining({
        conversation_id: "conversation",
        text: expect.stringContaining("sin registrar ni ejecutar código"),
      }),
    );
    expect(
      call.mock.calls.filter(
        ([, v]) =>
          v &&
          typeof v === "object" &&
          "tool" in v &&
          v.tool === "plugin_builder.create",
      ),
    ).toHaveLength(0);
  });
});
