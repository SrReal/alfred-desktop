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
test("a pending native publication keeps the saved connection ID for explicit credential reconciliation", async () => {
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save")
      throw new Error(
        "HA_CONNECTION_CREDENTIAL_PENDING:11111111-1111-4111-8111-111111111111",
      );
    return [];
  });
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Conexiones" }));
  fireEvent.change(await screen.findByLabelText("Tipo de conexión"), {
    target: { value: "homeassistant" },
  });
  fireEvent.change(screen.getByLabelText("Nombre de conexión"), {
    target: { value: "Owned pending HA" },
  });
  fireEvent.change(screen.getByLabelText("Credencial de conexión"), {
    target: { value: "owned_fictional_ha_token" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar conexión" }));
  expect(
    await screen.findByText(/La conexión quedó pendiente de credencial/),
  ).toBeVisible();
  mock.request.mockImplementation(async (op: string) =>
    op === "connections.save"
      ? { id: "11111111-1111-4111-8111-111111111111" }
      : [],
  );
  fireEvent.click(screen.getByRole("button", { name: "Guardar conexión" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith(
      "connections.save",
      expect.objectContaining({
        id: "11111111-1111-4111-8111-111111111111",
        module_id: "homeassistant",
        secret: "owned_fictional_ha_token",
      }),
    ),
  );
  await waitFor(() =>
    expect(screen.getByLabelText("Credencial de conexión")).toHaveValue(""),
  );
});
test("Home Assistant defaults to a closed read-only profile and clears another provider credential", async () => {
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Conexiones" }));
  fireEvent.change(await screen.findByLabelText("Tipo de conexión"), {
    target: { value: "git" },
  });
  fireEvent.change(screen.getByLabelText("Credencial de conexión"), {
    target: { value: "owned_fictional_previous_token" },
  });
  fireEvent.change(screen.getByLabelText("Tipo de conexión"), {
    target: { value: "homeassistant" },
  });
  expect(screen.getByLabelText("Credencial de conexión")).toHaveValue("");
  const config = JSON.parse(
    (screen.getByLabelText("Configuración de conexión") as HTMLTextAreaElement)
      .value,
  );
  expect(config).toEqual({
    profile_version: 1,
    base_url: "https://homeassistant.example",
    read_only: true,
    websocket: false,
    allow_insecure_http: false,
    allowed_entities: [],
    allowed_services: {},
    allowed_events: [],
  });
  expect(screen.getByText(/el token viajará sin cifrar/)).toBeVisible();
  expect(screen.getByText(/resultado físico de un dispositivo/)).toBeVisible();
  fireEvent.change(screen.getByLabelText("Nombre de conexión"), {
    target: { value: "Owned HA" },
  });
  fireEvent.change(screen.getByLabelText("Credencial de conexión"), {
    target: { value: "owned_fictional_ha_token" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar conexión" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("connections.save", {
      id: null,
      module_id: "homeassistant",
      name: "Owned HA",
      config,
      secret: "owned_fictional_ha_token",
    }),
  );
  await waitFor(() =>
    expect(screen.getByLabelText("Credencial de conexión")).toHaveValue(""),
  );
});
