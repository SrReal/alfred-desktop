import {
  act,
  render,
  screen,
  fireEvent,
  waitFor,
} from "@testing-library/react";
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
const ID = "11111111-1111-4111-8111-111111111111";
const SESSION = "22222222-2222-4222-8222-222222222222";
beforeEach(() => {
  history.replaceState({}, "", "/?surface=settings");
  mock.request.mockReset();
  mock.on.mockReset().mockResolvedValue(() => {});
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save") return { id: ID };
    if (op === "connections.oauth")
      return {
        session_id: SESSION,
        system_browser_launched: true,
        expires_in: 180,
      };
    if (op === "connections.oauth_status") return null;
    return [];
  });
});
test("Google resource selection is explicit and selected IDs never silently enlarge the saved local scope", async () => {
  let started = false;
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save") return { id: ID };
    if (op === "connections.oauth") {
      started = true;
      return { session_id: SESSION };
    }
    if (op === "connections.oauth_status")
      return started
        ? {
            phase: "ready",
            session_id: SESSION,
            picked_file_ids: ["owned_file", "owned_folder"],
          }
        : null;
    return [];
  });
  await selectDrive();
  await saveDrive();
  fireEvent.click(
    screen.getByRole("button", {
      name: "Autorizar y elegir archivos o carpetas",
    }),
  );
  await screen.findByText("Drive autorizado");
  expect(mock.request).toHaveBeenCalledWith("connections.oauth", {
    connection: ID,
    pick_resources: true,
  });
  expect(screen.getByLabelText("IDs elegidos en Google Drive")).toHaveValue(
    JSON.stringify(["owned_file", "owned_folder"], null, 2),
  );
  expect(
    mock.request.mock.calls.filter((c) => c[0] === "connections.save"),
  ).toHaveLength(1);
});
test("Resource selection requires a saved current profile", async () => {
  await selectDrive();
  expect(
    screen.queryByRole("button", {
      name: "Autorizar y elegir archivos o carpetas",
    }),
  ).toBeNull();
  expect(
    mock.request.mock.calls.filter((c) => c[0] === "connections.oauth"),
  ).toHaveLength(0);
});
async function selectDrive() {
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Conexiones" }));
  fireEvent.change(await screen.findByLabelText("Tipo de conexión"), {
    target: { value: "git" },
  });
  fireEvent.change(screen.getByLabelText("Credencial de conexión"), {
    target: { value: "owned_fictional_previous_token" },
  });
  fireEvent.change(screen.getByLabelText("Tipo de conexión"), {
    target: { value: "drive" },
  });
  fireEvent.change(screen.getByLabelText("Nombre de conexión"), {
    target: { value: "Owned Drive" },
  });
}
async function saveDrive() {
  fireEvent.click(screen.getByRole("button", { name: "Guardar conexión" }));
  return screen.findByRole("button", { name: "Autorizar Google Drive" });
}
test("Drive requires a saved explicit Desktop profile, defaults to read-only and never treats a previous token as its client secret", async () => {
  await selectDrive();
  expect(screen.getByLabelText("Credencial de conexión")).toHaveValue("");
  expect(
    screen.queryByRole("button", { name: "Autorizar Google Drive" }),
  ).toBeNull();
  const config = JSON.parse(
    (screen.getByLabelText("Configuración de conexión") as HTMLTextAreaElement)
      .value,
  );
  expect(config).toMatchObject({
    auth: "installed_app_pkce",
    client_secret_mode: "none",
    read_only: true,
    allowed_files: [],
    allowed_folders: [],
  });
  expect(config.token).toBeUndefined();
  await saveDrive();
  expect(mock.request).toHaveBeenCalledWith("connections.save", {
    id: null,
    module_id: "drive",
    name: "Owned Drive",
    config,
    secret: "",
    reauthorize: false,
  });
  fireEvent.click(
    screen.getByLabelText(
      "Reautorizar al cambiar el cliente o migrar esta conexión",
    ),
  );
  expect(
    screen.queryByRole("button", { name: "Autorizar Google Drive" }),
  ).toBeNull();
  await saveDrive();
  expect(mock.request).toHaveBeenCalledWith("connections.save", {
    id: ID,
    module_id: "drive",
    name: "Owned Drive",
    config,
    secret: "",
    reauthorize: true,
  });
  expect(
    screen.getByLabelText(
      "Reautorizar al cambiar el cliente o migrar esta conexión",
    ),
  ).not.toBeChecked();
});
test("Drive system-browser authorization uses public status and cancels only the current session", async () => {
  let cancelled = false;
  let started = false;
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save") return { id: ID };
    if (op === "connections.oauth") {
      started = true;
      return { session_id: SESSION, system_browser_launched: true };
    }
    if (op === "connections.oauth_cancel") {
      cancelled = true;
      return { cancelled: true };
    }
    if (op === "connections.oauth_status")
      return started
        ? {
            phase: cancelled ? "reauthorize" : "authorizing",
            session_id: SESSION,
          }
        : null;
    return [];
  });
  await selectDrive();
  fireEvent.click(await saveDrive());
  const cancel = await screen.findByRole("button", {
    name: "Cancelar autorización de Drive",
  });
  await waitFor(() => expect(cancel).toBeEnabled());
  expect(mock.request).toHaveBeenCalledWith("connections.oauth", {
    connection: ID,
  });
  expect(screen.queryByRole("link", { name: /consentimiento/i })).toBeNull();
  fireEvent.click(cancel);
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("connections.oauth_cancel", {
      connection: ID,
      session_id: SESSION,
    }),
  );
  await screen.findByText("Es necesario autorizar Drive");
  expect(
    screen.queryByRole("button", { name: "Cancelar autorización de Drive" }),
  ).toBeNull();
});
test("A late authorization result cannot select a session after switching provider", async () => {
  let resolve!: (value: { session_id: string }) => void;
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save") return { id: ID };
    if (op === "connections.oauth")
      return new Promise((done) => {
        resolve = done;
      });
    if (op === "connections.oauth_status") return null;
    return [];
  });
  await selectDrive();
  fireEvent.click(await saveDrive());
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("connections.oauth", {
      connection: ID,
    }),
  );
  fireEvent.change(screen.getByLabelText("Tipo de conexión"), {
    target: { value: "notion" },
  });
  const statusesBefore = mock.request.mock.calls.filter(
    (c) => c[0] === "connections.oauth_status",
  ).length;
  await act(async () => resolve({ session_id: SESSION }));
  expect(screen.getByLabelText("Tipo de conexión")).toHaveValue("notion");
  expect(
    screen.queryByRole("button", { name: "Cancelar autorización de Drive" }),
  ).toBeNull();
  expect(
    mock.request.mock.calls.filter((c) => c[0] === "connections.oauth_status"),
  ).toHaveLength(statusesBefore);
});
test("Drive ignores completion for another session and refreshes matching completion", async () => {
  let event!: (value: { connection: string; session_id: string }) => void;
  mock.on.mockImplementation(async (name: string, callback: typeof event) => {
    if (name === "alfredpet:oauth") event = callback;
    return () => {};
  });
  let phase = "authorizing";
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save") return { id: ID };
    if (op === "connections.oauth") return { session_id: SESSION };
    if (op === "connections.oauth_status")
      return { phase, session_id: SESSION };
    return [];
  });
  await selectDrive();
  await saveDrive();
  await screen.findByText(
    "Esperando el consentimiento en el navegador del sistema",
  );
  const before = mock.request.mock.calls.filter(
    (c) => c[0] === "connections.oauth_status",
  ).length;
  event({ connection: ID, session_id: "33333333-3333-4333-8333-333333333333" });
  expect(
    mock.request.mock.calls.filter((c) => c[0] === "connections.oauth_status"),
  ).toHaveLength(before);
  phase = "ready";
  event({ connection: ID, session_id: SESSION });
  await screen.findByText("Drive autorizado");
});
test("A status read from before authorization cannot overwrite its completed session", async () => {
  let resolve!: (value: { phase: string; session_id: string }) => void;
  let first = true;
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save") return { id: ID };
    if (op === "connections.oauth") return { session_id: SESSION };
    if (op === "connections.oauth_status") {
      if (first) {
        first = false;
        return new Promise((done) => {
          resolve = done;
        });
      }
      return { phase: "ready", session_id: SESSION };
    }
    return [];
  });
  await selectDrive();
  const authorize = await saveDrive();
  await waitFor(() => expect(first).toBe(false));
  fireEvent.click(authorize);
  await screen.findByText("Drive autorizado");
  await act(async () =>
    resolve({ phase: "uncertain", session_id: "owned_previous_session" }),
  );
  expect(screen.getByText("Drive autorizado")).toBeInTheDocument();
  expect(screen.queryByText(/Resultado incierto:/)).toBeNull();
});
test("A superseded session is reported without offering to cancel another actor's session", async () => {
  let started = false;
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save") return { id: ID };
    if (op === "connections.oauth") {
      started = true;
      return { session_id: SESSION };
    }
    if (op === "connections.oauth_status")
      return started
        ? {
            phase: "authorizing",
            session_id: "33333333-3333-4333-8333-333333333333",
          }
        : null;
    return [];
  });
  await selectDrive();
  fireEvent.click(await saveDrive());
  await screen.findByText("DRIVE_AUTH_SUPERSEDED");
  expect(
    screen.queryByRole("button", { name: "Cancelar autorización de Drive" }),
  ).toBeNull();
  expect(
    mock.request.mock.calls.filter((c) => c[0] === "connections.oauth_cancel"),
  ).toHaveLength(0);
});
test("A denied status read stops polling and clears an obsolete cancellation control", async () => {
  let started = false;
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save") return { id: ID };
    if (op === "connections.oauth") {
      started = true;
      return { session_id: SESSION };
    }
    if (op === "connections.oauth_status") {
      if (started) throw new Error("PRIVACY_DENIED");
      return null;
    }
    return [];
  });
  await selectDrive();
  fireEvent.click(await saveDrive());
  await screen.findByText("Estado de Drive no disponible");
  expect(screen.getByText(/PRIVACY_DENIED/)).toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "Cancelar autorización de Drive" }),
  ).toBeNull();
  const count = mock.request.mock.calls.filter(
    (c) => c[0] === "connections.oauth_status",
  ).length;
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 1150));
  });
  expect(
    mock.request.mock.calls.filter((c) => c[0] === "connections.oauth_status"),
  ).toHaveLength(count);
});
