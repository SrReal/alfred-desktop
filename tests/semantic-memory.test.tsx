import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { SemanticMemory } from "../src/ui/SemanticMemory";
const mock = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => mock);
const initial = {
  available: true,
  enabled: false,
  installed: false,
  model: "MiniLM",
  bytes: 127540686,
  dimensions: 384,
  indexed: 0,
  last_error: null,
  job: null,
};
beforeEach(() => {
  mock.request.mockReset();
  mock.on.mockReset();
  mock.on.mockResolvedValue(() => {});
});
test("download requires user opt-in and never auto-enables the model", async () => {
  mock.request.mockImplementation(async (operation: string) =>
    operation === "semantic.status" ? initial : [],
  );
  render(<SemanticMemory />);
  const download = await screen.findByRole("button", {
    name: "Descargar modelo",
  });
  expect(download).toBeDisabled();
  expect(mock.request).not.toHaveBeenCalledWith(
    "semantic.download",
    expect.anything(),
  );
  fireEvent.click(screen.getByRole("checkbox"));
  fireEvent.click(download);
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("semantic.download", {
      consent: true,
    }),
  );
  expect(mock.request).not.toHaveBeenCalledWith("settings.set", {
    key: "semantic_memory",
    value: true,
  });
});
test("progress can be cancelled and inference failures remain visible with lexical fallback", async () => {
  let event = () => {};
  let status = {
    ...initial,
    installed: true,
    enabled: true,
    job: {
      status: "running",
      phase: "index",
      completed: 2,
      total: 7,
      error: "",
    },
    last_error: "",
  };
  mock.on.mockImplementation(async (_: string, fn: () => void) => {
    event = fn;
    return () => {};
  });
  mock.request.mockImplementation(async (operation: string) => {
    if (operation === "semantic.status") return status;
    if (operation === "semantic.cancel")
      status = {
        ...status,
        job: { ...status.job, status: "cancelled" },
        last_error: "SEMANTIC_TIMEOUT",
      };
    return [];
  });
  render(<SemanticMemory />);
  await screen.findByText("Indexando: 2 / 7");
  fireEvent.click(screen.getByRole("button", { name: "Cancelar" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("semantic.cancel", {}),
  );
  act(() => event());
  await screen.findByRole("alert");
  expect(screen.getByRole("alert")).toHaveTextContent("SEMANTIC_TIMEOUT");
  expect(
    screen.getByText(/búsqueda por palabras sigue disponible/),
  ).toBeInTheDocument();
});
test("query passes explicit scope and exposes summary origin", async () => {
  mock.request.mockImplementation(async (operation: string) => {
    if (operation === "semantic.status")
      return { ...initial, installed: true, enabled: true };
    if (operation === "projects.list") return [{ id: "p", name: "Alfred" }];
    if (operation === "semantic.query")
      return [
        {
          id: "c",
          kind: "summary",
          title: "Base de datos",
          content: "MySQL producción",
          semantic_score: 0.36,
          fts_score: 0,
          sources: [{ conversation_id: "c" }],
        },
      ];
    return [];
  });
  render(<SemanticMemory />);
  await screen.findByRole("option", { name: "Alfred" });
  fireEvent.change(screen.getByLabelText("Proyecto de la búsqueda semántica"), {
    target: { value: "p" },
  });
  fireEvent.change(screen.getByLabelText("Consulta por significado"), {
    target: { value: "MariaDB" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Buscar" }));
  await screen.findByText("MySQL producción");
  expect(mock.request).toHaveBeenCalledWith("semantic.query", {
    query: "MariaDB",
    project_id: "p",
  });
  fireEvent.click(screen.getByRole("button", { name: "Abrir origen" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("conversations.open", {
      id: "c",
    }),
  );
});
