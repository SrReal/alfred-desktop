import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { MemoryAnalysis } from "../src/ui/MemoryAnalysis";
const mock = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => mock);
beforeEach(() => {
  mock.request.mockReset();
  mock.on.mockReset();
});
const candidate = {
  id: "candidate-1",
  conversation_id: "conversation-1",
  conversation_title: "Preferencias",
  project_id: "project-1",
  body: {
    title: "Ventanas",
    content: "Prefiere ventanas compactas",
    confidence: 0.6,
    rationale: "El usuario lo expresó",
    scope: "project",
    supersedes: "old-memory",
    evidence: [
      { message_id: "user-1", quote: "Prefiero las ventanas compactas." },
    ],
  },
};
test("memory proposals expose evidence and require explicit review", async () => {
  let pending = [candidate];
  mock.on.mockResolvedValue(() => {});
  mock.request.mockImplementation(async (operation: string) => {
    if (operation === "memory.candidates") return pending;
    if (operation === "memory.candidate_accept") {
      pending = [];
      return {};
    }
    return [];
  });
  render(<MemoryAnalysis />);
  await screen.findByText("Prefiere ventanas compactas");
  expect(
    screen.getByText("Prefiero las ventanas compactas."),
  ).toBeInTheDocument();
  expect(screen.getByText(/Propone sustituir/)).toBeInTheDocument();
  expect(mock.request).not.toHaveBeenCalledWith(
    "memory.candidate_accept",
    expect.anything(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Ver origen" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("conversations.open", {
      id: "conversation-1",
    }),
  );
  // Opening the source also refreshes the list; acceptance remains disabled
  // until that operation finishes, even when its first RPC already resolved.
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Aceptar propuesta" }),
    ).toBeEnabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Aceptar propuesta" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("memory.candidate_accept", {
      id: "candidate-1",
    }),
  );
  await waitFor(() =>
    expect(
      screen.queryByText("Prefiere ventanas compactas"),
    ).not.toBeInTheDocument(),
  );
});
test("analysis refreshes on events, cancels its conversation and retains errors", async () => {
  let event: () => void = () => {};
  mock.on.mockImplementation(async (_: string, callback: () => void) => {
    event = callback;
    return () => {};
  });
  let state = "running";
  mock.request.mockImplementation(async (operation: string) => {
    if (operation === "memory.analyses")
      return [
        {
          conversation_id: "conversation-1",
          title: "Preferencias",
          status: state,
          error_code: state === "error" ? "MEMORY_RESPONSE_SCHEMA" : null,
          summary_json: JSON.stringify({
            conversation_summary: "Resumen anterior",
            task_outcome: "Guardado",
            decisions: ["Compactas"],
            artifacts: ["Informe"],
          }),
        },
      ];
    if (operation === "memory.cancel_extraction") {
      state = "error";
      return {};
    }
    if (operation === "memory.candidates") return [candidate];
    if (operation === "memory.candidate_reject")
      throw new Error("CONVERSATION_NO_MEMORY");
    return [];
  });
  render(<MemoryAnalysis />);
  await screen.findByRole("button", { name: "Cancelar análisis" });
  expect(screen.getByText("Resumen anterior")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Cancelar análisis" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("memory.cancel_extraction", {
      conversation_id: "conversation-1",
    }),
  );
  act(() => event());
  await screen.findByText("MEMORY_RESPONSE_SCHEMA");
  expect(screen.getByText("Resumen anterior")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Rechazar propuesta" }));
  await waitFor(() =>
    expect(screen.getByRole("alert")).toHaveTextContent(
      "CONVERSATION_NO_MEMORY",
    ),
  );
  expect(screen.getByText("Prefiere ventanas compactas")).toBeInTheDocument();
});
