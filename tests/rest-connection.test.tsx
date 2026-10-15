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
    target: { value: "rest" },
  });
  fireEvent.change(screen.getByLabelText("Nombre de conexión"), {
    target: { value: "Owned REST" },
  });
}
test("REST template is closed, read-only and uses named private JSON references", async () => {
  await form();
  const config = JSON.parse(
    (screen.getByLabelText("Configuración de conexión") as HTMLTextAreaElement)
      .value,
  );
  expect(config).toMatchObject({
    profile_version: 1,
    base_url: "https://api.example.com/",
    allowed_hosts: ["api.example.com"],
    read_only: true,
    auth: { type: "none" },
    operations: { status: { method: "GET", response: "json", risk: "free" } },
  });
  expect(
    screen.queryByRole("button", { name: "Revisar OpenAPI" }),
  ).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Guardar conexión" }));
  await screen.findByRole("button", { name: "Revisar OpenAPI" });
  fireEvent.change(screen.getByLabelText("Configuración de conexión"), {
    target: {
      value: JSON.stringify({ ...config, openapi_path: "/other.json" }),
    },
  });
  expect(
    screen.queryByRole("button", { name: "Revisar OpenAPI" }),
  ).not.toBeInTheDocument();
});
test("pending REST native publication retains the connection ID for explicit secret re-entry", async () => {
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save")
      throw new Error(`REST_CONNECTION_CREDENTIAL_PENDING:${id}`);
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
        module_id: "rest",
        secret: '{"main":"owned_fictional"}',
      }),
    ),
  );
});
