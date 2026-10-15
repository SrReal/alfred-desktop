import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { Terminal } from "../src/ui/Terminal";
const mock = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => mock);
beforeEach(() => {
  mock.request.mockReset();
  mock.on.mockReset();
});
test("terminal streams only its run, cancels its process and retains output on failure", async () => {
  let event: (value: {
    id: string;
    channel: string;
    text?: string;
    status?: string;
  }) => void = () => {};
  mock.on.mockImplementation(async (_: string, callback: typeof event) => {
    event = callback;
    return () => {};
  });
  let finish: (value?: unknown) => void = () => {};
  let id = "";
  mock.request.mockImplementation(
    async (_: string, value: { tool: string; input: { id?: string } }) => {
      if (value.tool === "terminal.command") {
        id = value.input.id ?? "";
        return new Promise((_, reject) => {
          finish = reject;
        });
      }
      return [];
    },
  );
  render(<Terminal />);
  mock.request.mockResolvedValueOnce(["C:/fixtures"]);
  fireEvent.click(screen.getByLabelText("Carpeta del terminal"));
  await waitFor(() =>
    expect(screen.getByLabelText("Carpeta del terminal")).toHaveAttribute(
      "title",
      "C:/fixtures",
    ),
  );
  fireEvent.change(screen.getByLabelText("Comando PowerShell"), {
    target: { value: "Start-Sleep -Seconds 10" },
  });
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Ejecutar comando" }),
    ).toBeEnabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Ejecutar comando" }));
  await waitFor(() => expect(id).toMatch(/^terminal-/));
  expect(
    screen.getByRole("button", { name: "Cancelar comando" }),
  ).toBeDisabled();
  act(() => {
    event({ id: "other-run", channel: "stdout", text: "foreign output" });
    event({ id, channel: "status", status: "running" });
    event({ id, channel: "stdout", text: "first chunk" });
  });
  expect(screen.getByLabelText("Salida del terminal")).toHaveTextContent(
    "first chunk",
  );
  expect(screen.queryByText(/foreign output/)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Cancelar comando" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("tools.execute", {
      tool: "terminal.cancel",
      input: { id },
    }),
  );
  await act(async () => {
    finish(new Error("TERMINAL_CANCELLED"));
  });
  expect(screen.getByRole("alert")).toHaveTextContent("TERMINAL_CANCELLED");
  expect(screen.getByLabelText("Salida del terminal")).toHaveTextContent(
    "first chunk",
  );
  expect(
    screen.getByRole("button", { name: "Ejecutar comando" }),
  ).toBeEnabled();
});
test("profiles load, select and save permitted environment names", async () => {
  mock.on.mockResolvedValue(() => {});
  mock.request.mockImplementation(async (_: string, value: { tool: string }) =>
    value.tool === "terminal.profiles"
      ? [{ id: "profile-1", name: "Local", shell: "powershell7" }]
      : {},
  );
  render(<Terminal />);
  fireEvent.click(screen.getByRole("button", { name: "Cargar perfiles" }));
  await screen.findByRole("option", { name: "Local · powershell7" });
  fireEvent.change(screen.getByLabelText("Nombre del perfil"), {
    target: { value: "Fixture" },
  });
  fireEvent.change(screen.getByLabelText("Variables permitidas del perfil"), {
    target: { value: "DEMO_VALUE, PROJECT_NAME" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar perfil" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("tools.execute", {
      tool: "terminal.profile_save",
      input: {
        name: "Fixture",
        body: { shell: "auto", env_allowlist: ["DEMO_VALUE", "PROJECT_NAME"] },
      },
    }),
  );
});
