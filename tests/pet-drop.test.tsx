import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { PetDrop } from "../src/ui/PetDrop";
const mock = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => mock);
let pending: {
  items: {
    id: string;
    name: string;
    path: string;
    error: string | null;
    created_at: string;
  }[];
  last_error: string | null;
};
let changed: (() => void) | undefined;
beforeEach(() => {
  pending = {
    items: [
      {
        id: "file-1",
        name: "own.txt",
        path: "C:\\owned\\own.txt",
        error: null,
        created_at: "2026-10-03T12:00:00Z",
      },
    ],
    last_error: null,
  };
  mock.request.mockReset();
  mock.on.mockReset();
  changed = undefined;
  mock.on.mockImplementation(async (_event, callback) => {
    changed = callback;
    return () => {};
  });
  mock.request.mockImplementation(async (operation) => {
    if (operation === "pet_drop.list") return structuredClone(pending);
    if (operation === "conversations.create") return { id: "created" };
    if (operation === "pet_drop.attach") {
      pending.items = [];
      return {
        conversation_id: "conversation-1",
        attached: [{ id: "file-1" }],
        failed: [],
      };
    }
    if (operation === "pet_drop.dismiss") {
      pending.items = [];
      return { original_files_preserved: true };
    }
    return {};
  });
});
function show(busy = false) {
  const attached = vi.fn();
  render(
    <PetDrop
      conversations={[{ id: "conversation-1", title: "Existing" }]}
      busy={busy}
      onAttached={attached}
    />,
  );
  return attached;
}
test("a malformed pending response produces a visible error without crashing the surface", async () => {
  mock.request.mockResolvedValue([]);
  show();
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "DROP_QUEUE_RESPONSE",
  );
  expect(
    screen.getByRole("region", { name: "Archivos soltados sobre AlfredPet" }),
  ).toBeInTheDocument();
});
test("native pending files are reviewed before attaching to the chosen conversation", async () => {
  const attached = show();
  fireEvent.click(await screen.findByRole("checkbox"));
  expect(mock.request).not.toHaveBeenCalledWith(
    "pet_drop.attach",
    expect.anything(),
  );
  fireEvent.change(screen.getByRole("combobox"), {
    target: { value: "conversation-1" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Adjuntar selección" }));
  expect(await screen.findByRole("status")).toHaveTextContent(
    "originales se conservan",
  );
  expect(mock.request).toHaveBeenCalledWith("pet_drop.attach", {
    ids: ["file-1"],
    conversation_id: "conversation-1",
  });
  expect(attached).toHaveBeenCalledWith("conversation-1", false);
  expect(mock.request).not.toHaveBeenCalledWith("conversations.create");
  expect(mock.request).not.toHaveBeenCalledWith("chat.send", expect.anything());
});
test("analysis prepares a draft through the callback and does not send a turn", async () => {
  const attached = show();
  fireEvent.click(await screen.findByRole("checkbox"));
  fireEvent.click(
    screen.getByRole("button", { name: "Adjuntar y preparar análisis" }),
  );
  expect(await screen.findByRole("status")).toHaveTextContent("pulsa Enviar");
  expect(mock.request).toHaveBeenCalledWith("conversations.create");
  expect(mock.request).toHaveBeenCalledWith("pet_drop.attach", {
    ids: ["file-1"],
    conversation_id: "created",
  });
  expect(attached).toHaveBeenCalledWith("conversation-1", true);
  expect(mock.request).not.toHaveBeenCalledWith("chat.send", expect.anything());
});
test("partial errors remain visible and retry does not create another conversation", async () => {
  mock.request.mockImplementation(async (operation) => {
    if (operation === "pet_drop.list") return pending;
    if (operation === "conversations.create") return { id: "created" };
    return {
      conversation_id: "created",
      attached: [],
      failed: [{ id: "file-1", name: "own.txt", error: "PATH_SCOPE" }],
    };
  });
  show();
  fireEvent.click(await screen.findByRole("checkbox"));
  fireEvent.click(screen.getByRole("button", { name: "Adjuntar selección" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("PATH_SCOPE");
  fireEvent.click(screen.getByRole("button", { name: "Adjuntar selección" }));
  await act(async () => {});
  expect(
    mock.request.mock.calls.filter(
      ([operation]) => operation === "conversations.create",
    ),
  ).toHaveLength(1);
});
test("dismissal preserves files and a running conversation prevents attachment actions", async () => {
  show(true);
  expect(await screen.findByRole("checkbox")).toBeDisabled();
  expect(
    screen.getByRole("button", { name: "Adjuntar selección" }),
  ).toBeDisabled();
});
test("new native events refresh the queue and dismiss only selected file identifiers", async () => {
  const attached = show();
  fireEvent.click(await screen.findByRole("checkbox"));
  fireEvent.click(screen.getByRole("button", { name: "Descartar selección" }));
  expect(await screen.findByRole("status")).toHaveTextContent(
    "originales se conservan",
  );
  expect(mock.request).toHaveBeenCalledWith("pet_drop.dismiss", {
    ids: ["file-1"],
  });
  expect(attached).not.toHaveBeenCalled();
  pending.items = [
    {
      id: "file-2",
      name: "new.txt",
      path: "C:\\owned\\new.txt",
      error: null,
      created_at: "2026-10-03T12:00:00Z",
    },
  ];
  await act(async () => {
    changed?.();
  });
  expect(await screen.findByRole("checkbox")).not.toBeChecked();
  expect(screen.getByText("new.txt")).toBeInTheDocument();
});
