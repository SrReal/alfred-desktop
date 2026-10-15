import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { request } from "../src/api";
import { MacroRun } from "../src/ui/MacroRun";
vi.mock("../src/api", () => ({ request: vi.fn() }));
const call = vi.mocked(request);
const fingerprint = "a".repeat(64);
const definition = {
  description: "Preparar una copia revisada",
  error_strategy: "stop",
  parameters: {
    destination: { type: "string", required: true },
    copies: { type: "integer", default: 2 },
    enabled: { type: "boolean" },
  },
  steps: [
    {
      tool: "files.copy",
      rollback_hint: "Revisar y borrar la copia si procede",
    },
  ],
};
beforeEach(() => {
  call.mockReset();
});
describe("macro review", () => {
  it("saves reviewed typed values as a routine without executing the macro", async () => {
    call.mockResolvedValueOnce({
      id: "one",
      name: "Copias",
      fingerprint,
      enabled: true,
      definition,
    });
    const saved = vi.fn();
    render(
      <MacroRun
        id="one"
        onClose={vi.fn()}
        onResult={vi.fn()}
        onSavedRoutine={saved}
      />,
    );
    await screen.findByText("Copias");
    fireEvent.change(screen.getByLabelText(/destination/), {
      target: { value: "D:\\Owned\\copy.txt" },
    });
    fireEvent.change(screen.getByLabelText(/enabled/), {
      target: { value: "false" },
    });
    call.mockResolvedValueOnce({ saved: true });
    fireEvent.click(screen.getByText("Guardar como rutina"));
    await waitFor(() => expect(saved).toHaveBeenCalledOnce());
    expect(call).toHaveBeenLastCalledWith("routines.save", {
      name: "Copias",
      macroId: "one",
      fingerprint,
      body: { destination: "D:\\Owned\\copy.txt", copies: 2, enabled: false },
    });
    expect(
      call.mock.calls.some(
        ([, input]) =>
          input &&
          typeof input === "object" &&
          "tool" in input &&
          input.tool === "macros.run",
      ),
    ).toBe(false);
  });
  it("uses a native path picker for parameters bound to file inputs", async () => {
    call.mockResolvedValueOnce({
      id: "one",
      name: "Lectura",
      fingerprint,
      enabled: true,
      definition: {
        ...definition,
        parameters: { source: { type: "string", required: true } },
        steps: [
          { tool: "files.read", input: { path: { $parameter: "source" } } },
        ],
      },
    });
    render(<MacroRun id="one" onClose={vi.fn()} onResult={vi.fn()} />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Seleccionar source" }),
    );
    expect(call).toHaveBeenLastCalledWith(
      "paths.pick",
      expect.objectContaining({ mode: "file" }),
    );
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });
  it("shows the contract and collects typed values before an explicit run", async () => {
    call.mockResolvedValueOnce({
      id: "one",
      name: "Copias",
      fingerprint,
      enabled: true,
      definition,
    });
    const onResult = vi.fn();
    render(<MacroRun id="one" onClose={vi.fn()} onResult={onResult} />);
    await screen.findByText("Copias");
    expect(call).toHaveBeenCalledTimes(1);
    expect(screen.getByText(/Reversión sugerida:/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText(/destination/), {
      target: { value: "D:\\Owned\\copied.txt" },
    });
    fireEvent.change(screen.getByLabelText(/enabled/), {
      target: { value: "false" },
    });
    call.mockResolvedValueOnce({ complete: true });
    fireEvent.click(screen.getByText("Revisar y ejecutar macro"));
    await waitFor(() =>
      expect(onResult).toHaveBeenCalledWith({ complete: true }),
    );
    expect(call).toHaveBeenLastCalledWith("tools.execute", {
      tool: "macros.run",
      input: {
        id: "one",
        expected: { fingerprint },
        body: {
          destination: "D:\\Owned\\copied.txt",
          copies: 2,
          enabled: false,
        },
      },
    });
    expect(call).toHaveBeenCalledTimes(2);
  });
  it("rejects missing required values and never retries a stale run", async () => {
    call.mockResolvedValueOnce({
      id: "one",
      name: "Copias",
      fingerprint,
      enabled: true,
      definition,
    });
    render(<MacroRun id="one" onClose={vi.fn()} onResult={vi.fn()} />);
    await screen.findByText("Copias");
    fireEvent.click(screen.getByText("Revisar y ejecutar macro"));
    await screen.findByText(/Falta el parámetro destination/);
    expect(call).toHaveBeenCalledTimes(1);
    fireEvent.change(screen.getByLabelText(/destination/), {
      target: { value: "D:\\Owned\\copy.txt" },
    });
    call.mockRejectedValueOnce("MACRO_CHANGED_REVIEW_REQUIRED");
    fireEvent.click(screen.getByText("Revisar y ejecutar macro"));
    await screen.findByText("MACRO_CHANGED_REVIEW_REQUIRED");
    expect(call).toHaveBeenCalledTimes(2);
  });
  it("ignores a late contract response after changing the selected macro", async () => {
    let resolveFirst!: (value: unknown) => void;
    call.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveFirst = resolve;
        }),
    );
    const view = render(
      <MacroRun id="one" onClose={vi.fn()} onResult={vi.fn()} />,
    );
    call.mockResolvedValueOnce({
      id: "two",
      name: "Segunda",
      fingerprint,
      enabled: true,
      definition: { ...definition, parameters: {} },
    });
    view.rerender(<MacroRun id="two" onClose={vi.fn()} onResult={vi.fn()} />);
    await screen.findByText("Segunda");
    resolveFirst({
      id: "one",
      name: "Primera",
      fingerprint,
      enabled: true,
      definition,
    });
    await waitFor(() =>
      expect(screen.queryByText("Primera")).not.toBeInTheDocument(),
    );
    expect(call).toHaveBeenCalledTimes(2);
  });
});
