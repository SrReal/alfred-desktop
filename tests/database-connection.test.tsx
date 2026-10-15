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
beforeEach(() => {
  history.replaceState({}, "", "/?surface=settings");
  mock.request.mockReset();
  mock.on.mockResolvedValue(() => {});
  mock.request.mockImplementation(async (op: string) =>
    op === "connections.save"
      ? { id: "11111111-1111-4111-8111-111111111111" }
      : [],
  );
});
test("database engine templates require explicit TLS and clear an old credential", async () => {
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Conexiones" }));
  fireEvent.change(await screen.findByLabelText("Tipo de conexión"), {
    target: { value: "database" },
  });
  fireEvent.change(screen.getByLabelText("Nombre de conexión"), {
    target: { value: "Owned database" },
  });
  fireEvent.change(screen.getByLabelText("Credencial de conexión"), {
    target: { value: "owned_previous_password" },
  });
  fireEvent.change(
    screen.getByLabelText("Plantilla de motor de base de datos"),
    { target: { value: "sqlserver" } },
  );
  expect(screen.getByLabelText("Credencial de conexión")).toHaveValue("");
  const config = JSON.parse(
    (screen.getByLabelText("Configuración de conexión") as HTMLTextAreaElement)
      .value,
  );
  expect(config).toMatchObject({
    engine: "sqlserver",
    read_only: true,
    ssl: "verify_full",
    port: 1433,
    driver: "ODBC Driver 18 for SQL Server",
  });
  expect(config.password).toBeUndefined();
  fireEvent.click(screen.getByRole("button", { name: "Guardar conexión" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("connections.save", {
      id: null,
      module_id: "database",
      name: "Owned database",
      config,
      secret: "",
    }),
  );
});
