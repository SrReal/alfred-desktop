import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { TasksPanel } from "../src/ui/TasksPanel";
import type { Task } from "../src/types";
const mock = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => mock);
let changed: (() => void) | undefined;
let rows: Task[];
function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "own-task",
    title: "Own task",
    description: "Original description",
    status: "queued",
    due_at: "2026-01-31T10:00:00.321Z",
    revision: 3,
    recurrence: {
      version: 1,
      frequency: "monthly",
      interval: 1,
      anchor: "2026-01-31T10:00:00.321Z",
    },
    recurrence_error: null,
    project_id: "own-project",
    project_name: "Project fixture",
    conversation_id: "own-conversation",
    conversation_title: "Conversation fixture",
    updated_at: "2026-01-01T00:00:00.000Z",
    ...overrides,
  };
}
beforeEach(() => {
  changed = undefined;
  rows = [];
  mock.request.mockReset();
  mock.on.mockReset();
  mock.on.mockImplementation(async (_event, callback) => {
    changed = callback;
    return () => {};
  });
  mock.request.mockImplementation(async (operation, input) => {
    if (operation === "projects.list")
      return [{ id: "own-project", name: "Project fixture" }];
    if (operation === "conversations.list")
      return [{ id: "own-conversation", title: "Conversation fixture" }];
    if (operation === "tools.execute" && input.tool === "tasks.list")
      return structuredClone(
        rows.slice(input.input.offset ?? 0, (input.input.offset ?? 0) + 50),
      );
    if (operation === "tools.execute" && input.tool === "tasks.history")
      return [
        {
          id: "run",
          status: "succeeded",
          ended_at: "2026-02-28T12:00:00Z",
          scheduled_for: "2026-01-31T10:00:00Z",
          error: null,
        },
      ];
    return {};
  });
});
async function show() {
  render(<TasksPanel />);
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Actualizar tareas" }),
    ).toBeEnabled(),
  );
}
test("creates an explicit recurrence with description and selected links and sends the local instant as UTC", async () => {
  await show();
  fireEvent.change(screen.getByLabelText("Nueva tarea"), {
    target: { value: "Monthly fixture" },
  });
  fireEvent.change(screen.getByLabelText("Descripción de tarea"), {
    target: { value: "Review this fixture" },
  });
  fireEvent.change(screen.getByLabelText("Repetición de tarea"), {
    target: { value: "monthly" },
  });
  expect(screen.getByRole("button", { name: "Crear" })).toBeDisabled();
  fireEvent.change(screen.getByLabelText("Fecha del recordatorio"), {
    target: { value: "2026-01-31T10:00:00.321" },
  });
  fireEvent.change(screen.getByLabelText("Intervalo de repetición"), {
    target: { value: "2" },
  });
  fireEvent.change(screen.getByLabelText("Proyecto de tarea"), {
    target: { value: "own-project" },
  });
  fireEvent.change(screen.getByLabelText("Conversación de tarea"), {
    target: { value: "own-conversation" },
  });
  expect(screen.getByText(/puede cambiar la hora local/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Crear" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("tools.execute", {
      tool: "tasks.create",
      input: {
        title: "Monthly fixture",
        description: "Review this fixture",
        due_at: new Date("2026-01-31T10:00:00.321").toISOString(),
        recurrence: { frequency: "monthly", interval: 2 },
        project_id: "own-project",
        conversation_id: "own-conversation",
      },
    }),
  );
  await waitFor(() =>
    expect(screen.getByLabelText("Nueva tarea")).toHaveValue(""),
  );
});
test("edits the existing revision without losing milliseconds and permits unlinking and clearing recurrence", async () => {
  rows = [task()];
  await show();
  fireEvent.click(screen.getByRole("button", { name: "Editar" }));
  expect(screen.getByLabelText("Descripción de tarea")).toHaveValue(
    "Original description",
  );
  fireEvent.change(screen.getByLabelText("Nueva tarea"), {
    target: { value: "Edited task" },
  });
  fireEvent.change(screen.getByLabelText("Proyecto de tarea"), {
    target: { value: "" },
  });
  fireEvent.change(screen.getByLabelText("Conversación de tarea"), {
    target: { value: "" },
  });
  fireEvent.change(screen.getByLabelText("Repetición de tarea"), {
    target: { value: "none" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar tarea" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("tools.execute", {
      tool: "tasks.update",
      input: {
        id: "own-task",
        expected_revision: 3,
        title: "Edited task",
        description: "Original description",
        due_at: "2026-01-31T10:00:00.321Z",
        recurrence: null,
        project_id: null,
        conversation_id: null,
      },
    }),
  );
});
test("completion uses the displayed revision and duplicate actions are blocked while awaiting the result", async () => {
  rows = [task()];
  await show();
  const previous = mock.request.getMockImplementation()!;
  let release: ((value: unknown) => void) | undefined;
  mock.request.mockImplementation((operation, input) =>
    input?.tool === "tasks.complete"
      ? new Promise((resolve) => {
          release = resolve;
        })
      : previous(operation, input),
  );
  const complete = screen.getByRole("button", {
    name: "Completar y programar siguiente",
  });
  fireEvent.click(complete);
  fireEvent.click(complete);
  expect(screen.getByRole("button", { name: "Borrar" })).toBeDisabled();
  expect(
    mock.request.mock.calls.filter(
      (call) => call[1]?.tool === "tasks.complete",
    ),
  ).toHaveLength(1);
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "tasks.complete",
    input: { id: "own-task", expected_revision: 3 },
  });
  rows = [task({ revision: 4, due_at: "2026-02-28T10:00:00.321Z" })];
  await act(async () => release?.({}));
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Borrar" })).toBeEnabled(),
  );
});
test("external revision changes require review and failed edits retain the draft and visible error", async () => {
  rows = [task()];
  await show();
  fireEvent.click(screen.getByRole("button", { name: "Editar" }));
  fireEvent.change(screen.getByLabelText("Nueva tarea"), {
    target: { value: "My draft" },
  });
  rows = [task({ revision: 4, title: "External edit" })];
  await act(async () => changed?.());
  expect(await screen.findByRole("status")).toHaveTextContent(
    "La tarea cambió",
  );
  expect(screen.getByRole("button", { name: "Guardar tarea" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Editar" }));
  expect(screen.getByLabelText("Nueva tarea")).toHaveValue("External edit");
  fireEvent.change(screen.getByLabelText("Nueva tarea"), {
    target: { value: "Retry draft" },
  });
  const previous = mock.request.getMockImplementation()!;
  mock.request.mockImplementation((operation, input) =>
    input?.tool === "tasks.update"
      ? Promise.reject(new Error("TASK_CHANGED: reload before retrying"))
      : previous(operation, input),
  );
  fireEvent.click(screen.getByRole("button", { name: "Guardar tarea" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("TASK_CHANGED");
  expect(screen.getByLabelText("Nueva tarea")).toHaveValue("Retry draft");
});
test("shows persisted run history, opens linked conversations and sends deletion through its revision contract", async () => {
  rows = [task()];
  await show();
  fireEvent.click(
    screen.getByRole("button", {
      name: "Abrir conversación: Conversation fixture",
    }),
  );
  expect(mock.request).toHaveBeenCalledWith("conversations.open", {
    id: "own-conversation",
  });
  fireEvent.click(screen.getByRole("button", { name: "Ver historial" }));
  const history = await screen.findByRole("region", {
    name: "Historial de tarea",
  });
  expect(within(history).getByText(/succeeded/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Borrar" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("tools.execute", {
      tool: "tasks.delete",
      input: { id: "own-task", expected_revision: 3 },
    }),
  );
});
test("loads bounded pages and reopening is an explicit edit with the displayed revision", async () => {
  rows = Array.from({ length: 53 }, (_, index) =>
    task({
      id: `task-${index}`,
      title: `Page ${index}`,
      status: "succeeded",
      recurrence: null,
    }),
  );
  await show();
  expect(screen.queryByText("Page 52")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Cargar más tareas" }));
  await screen.findByText("Page 52");
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "tasks.list",
    input: { offset: 50 },
  });
  expect(
    screen.queryByRole("button", { name: "Cargar más tareas" }),
  ).not.toBeInTheDocument();
  fireEvent.click(screen.getAllByRole("button", { name: "Reabrir" })[0]);
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("tools.execute", {
      tool: "tasks.update",
      input: { id: "task-0", expected_revision: 3, reopen: true },
    }),
  );
});
test("invalid list response is visible without rendering invented tasks", async () => {
  const previous = mock.request.getMockImplementation()!;
  mock.request.mockImplementation((operation, input) =>
    input?.tool === "tasks.list"
      ? Promise.resolve({})
      : previous(operation, input),
  );
  render(<TasksPanel />);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "TASK_LIST_RESPONSE",
  );
  expect(screen.queryByText("Own task")).not.toBeInTheDocument();
});
