import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { App } from "../src/ui/App";
const mock = vi.hoisted(() => ({
  request: vi.fn(),
  on: vi.fn(),
  show: vi.fn(),
}));
vi.mock("../src/api", () => ({ ...mock, desktop: false }));
vi.mock("@tauri-apps/plugin-autostart", () => ({
  enable: vi.fn(),
  disable: vi.fn(),
  isEnabled: vi.fn().mockResolvedValue(false),
}));
const id = "11111111-1111-4111-8111-111111111111";
beforeEach(() => {
  history.replaceState({}, "", "/?surface=settings");
  mock.request.mockReset();
  mock.on.mockResolvedValue(() => {});
  mock.request.mockImplementation(async (op: string) =>
    op === "connections.save" ? { id } : [],
  );
});
async function form() {
  render(<App />);
  fireEvent.click(screen.getByRole("button", { name: "Conexiones" }));
  fireEvent.change(await screen.findByLabelText("Tipo de conexión"), {
    target: { value: "mcp" },
  });
  fireEvent.change(screen.getByLabelText("Nombre de conexión"), {
    target: { value: "Owned MCP" },
  });
}
test("MCP has closed templates and private reference instructions; edits remove saved inspector", async () => {
  await form();
  const config = JSON.parse(
    (screen.getByLabelText("Configuración de conexión") as HTMLTextAreaElement)
      .value,
  );
  expect(config).toMatchObject({
    profile_version: 1,
    transport: "stdio",
    auth: { type: "none" },
    permissions: { tools: {}, resources: [], prompts: {} },
  });
  expect(config.command).toBeUndefined();
  expect(screen.getByRole("button", { name: "Ejecutable MCP" })).toBeEnabled();
  expect(
    screen.getByRole("button", { name: "Añadir archivo a argumentos MCP" }),
  ).toBeEnabled();
  expect(
    screen.queryByRole("button", { name: "Comprobar servidor MCP" }),
  ).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Plantilla MCP HTTP" }));
  expect(
    JSON.parse(
      (
        screen.getByLabelText(
          "Configuración de conexión",
        ) as HTMLTextAreaElement
      ).value,
    ),
  ).toMatchObject({ transport: "streamable_http", endpoint: "/mcp" });
  fireEvent.click(screen.getByRole("button", { name: "Guardar conexión" }));
  await screen.findByRole("button", { name: "Comprobar servidor MCP" });
  fireEvent.change(screen.getByLabelText("Credencial de conexión"), {
    target: { value: '{"main":"owned_fictional"}' },
  });
  expect(
    screen.queryByRole("button", { name: "Comprobar servidor MCP" }),
  ).not.toBeInTheDocument();
});
test("pending MCP credential publication retains ID and requests explicit re-entry", async () => {
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save")
      throw new Error(`MCP_CONNECTION_CREDENTIAL_PENDING:${id}`);
    return [];
  });
  await form();
  fireEvent.change(screen.getByLabelText("Credencial de conexión"), {
    target: { value: '{"main":"owned_fictional"}' },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar conexión" }));
  await screen.findByText(/Introduce de nuevo el mapa JSON/);
  mock.request.mockImplementation(async (op: string) =>
    op === "connections.save" ? { id } : [],
  );
  fireEvent.click(screen.getByRole("button", { name: "Guardar conexión" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith(
      "connections.save",
      expect.objectContaining({
        id,
        module_id: "mcp",
        secret: '{"main":"owned_fictional"}',
      }),
    ),
  );
});
