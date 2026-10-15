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
test("Notion template requires explicit IDs, defaults to read-only and clears another provider token", async () => {
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Conexiones" }));
  fireEvent.change(await screen.findByLabelText("Tipo de conexión"), {
    target: { value: "git" },
  });
  fireEvent.change(screen.getByLabelText("Credencial de conexión"), {
    target: { value: "owned_fictional_previous_token" },
  });
  fireEvent.change(screen.getByLabelText("Tipo de conexión"), {
    target: { value: "notion" },
  });
  expect(screen.getByLabelText("Credencial de conexión")).toHaveValue("");
  const config = JSON.parse(
    (screen.getByLabelText("Configuración de conexión") as HTMLTextAreaElement)
      .value,
  );
  expect(config).toMatchObject({
    auth: "integration_token",
    api_version: "2025-09-03",
    read_only: true,
    allowed_databases: [],
    allowed_data_sources: [],
  });
  expect(config.allowed_pages).toHaveLength(1);
  expect(config.base_url).toBeUndefined();
  expect(config.token).toBeUndefined();
  fireEvent.change(screen.getByLabelText("Nombre de conexión"), {
    target: { value: "Owned Notion" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar conexión" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("connections.save", {
      id: null,
      module_id: "notion",
      name: "Owned Notion",
      config,
      secret: "",
    }),
  );
});
