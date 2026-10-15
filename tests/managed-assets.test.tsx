import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import {
  AssetSettings,
  CaptureAsset,
  ConversationAttachments,
} from "../src/ui/ManagedAssets";
const mock = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => mock);
const asset = {
  id: "capture-1",
  label: "Captura",
  state: "ready",
  class: "capture",
  width: 800,
  height: 450,
  size: 1200,
  expires_at: "2026-10-04T12:00:00Z",
  reference_count: 0,
  thumbnail_data_url: "data:image/png;base64,YQ==",
};
let events: Record<string, (() => void)[]>;
beforeEach(() => {
  events = {};
  mock.request.mockReset();
  mock.on.mockReset();
  mock.on.mockImplementation(async (event, callback) => {
    (events[event] ??= []).push(callback);
    return () => {};
  });
  mock.request.mockImplementation(async (operation, input = {}) => {
    if (operation === "conversations.list")
      return [{ id: "conversation-1", title: "Mi conversación" }];
    if (operation === "settings.get") return [];
    if (operation === "assets.get") return asset;
    if (input.tool === "vision.cleanup") return { expired: 2, blocked: [] };
    return {};
  });
});
test("capture previews use verified bytes and attaching retains the chosen conversation", async () => {
  const deleted = vi.fn();
  render(<CaptureAsset id={asset.id} onDeleted={deleted} />);
  expect(await screen.findByAltText("Vista previa de Captura")).toHaveAttribute(
    "src",
    asset.thumbnail_data_url,
  );
  fireEvent.change(screen.getByRole("combobox"), {
    target: { value: "conversation-1" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Adjuntar captura" }));
  expect(await screen.findByRole("status")).toHaveTextContent("Se conservará");
  expect(mock.request).toHaveBeenCalledWith("attachments.add", {
    conversation_id: "conversation-1",
    asset_id: asset.id,
  });
  expect(deleted).not.toHaveBeenCalled();
  expect(mock.request).not.toHaveBeenCalledWith("assets.delete", {
    id: asset.id,
  });
});
test("privacy events clear the preview and reject a late response from before revocation", async () => {
  let late: ((value: unknown) => void) | undefined;
  mock.request.mockImplementation(async (operation, input) => {
    if (operation === "conversations.list") return [];
    if (operation === "assets.get")
      return new Promise((resolve) => {
        late = resolve;
      });
    return {};
  });
  render(<CaptureAsset id={asset.id} onDeleted={() => {}} />);
  await act(async () => {});
  const pending = late;
  mock.request.mockImplementation(async (operation) => {
    if (operation === "conversations.list") return [];
    throw new Error("PRIVACY_DENIED");
  });
  await act(async () => {
    events["alfredpet:settings"].forEach((callback) => callback());
  });
  await act(async () => {
    pending?.(asset);
  });
  expect(screen.queryByRole("img")).not.toBeInTheDocument();
  expect(screen.getByRole("alert")).toHaveTextContent("PRIVACY_DENIED");
});
test("an attached capture cannot be deleted and untrusted preview URLs are ignored", async () => {
  mock.request.mockImplementation(async (operation) =>
    operation === "conversations.list"
      ? []
      : {
          ...asset,
          reference_count: 1,
          thumbnail_data_url: "https://example.invalid/image.png",
        },
  );
  render(<CaptureAsset id={asset.id} onDeleted={() => {}} />);
  expect(
    await screen.findByText("Conservada en conversación"),
  ).toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "Solicitar borrar captura" }),
  ).toBeDisabled();
  expect(screen.queryByRole("img")).not.toBeInTheDocument();
});
test("removing a legacy attachment is explicit and reports preservation of its original file", async () => {
  let rows = [
    {
      id: "attachment-1",
      asset_id: null,
      path: "C:\\own\\legacy.png",
      size: 1024,
      metadata_json: "{}",
    },
  ];
  mock.request.mockImplementation(async (operation) => {
    if (operation === "attachments.list") return rows;
    if (operation === "attachments.remove") {
      rows = [];
      return { legacy_file_retained: true };
    }
    return {};
  });
  render(<ConversationAttachments conversationId="conversation-1" />);
  expect(await screen.findByText("legacy.png · 1.0 KiB")).toBeInTheDocument();
  expect(mock.request).not.toHaveBeenCalledWith(
    "attachments.remove",
    expect.anything(),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Retirar de conversación" }),
  );
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Su archivo original se conserva",
  );
  expect(mock.request).toHaveBeenCalledWith("attachments.remove", {
    conversation_id: "conversation-1",
    id: "attachment-1",
  });
});
test("retention validates bounds and cleanup runs only after an explicit request", async () => {
  render(<AssetSettings />);
  await act(async () => {});
  expect(mock.request).not.toHaveBeenCalledWith(
    "tools.execute",
    expect.objectContaining({ tool: "vision.cleanup" }),
  );
  fireEvent.change(screen.getByLabelText("Conservación en minutos"), {
    target: { value: "1" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar conservación" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("5 a 43200");
  expect(mock.request).not.toHaveBeenCalledWith(
    "settings.set",
    expect.anything(),
  );
  fireEvent.change(screen.getByLabelText("Conservación en minutos"), {
    target: { value: "60" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar conservación" }));
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Conservación guardada",
  );
  expect(mock.request).toHaveBeenCalledWith("settings.set", {
    key: "asset_lifecycle",
    value: {
      ttl_seconds: 3600,
      max_bytes: 512 * 1024 * 1024,
      cleanup_enabled: true,
    },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Solicitar limpiar caducados" }),
  );
  expect(await screen.findByRole("status")).toHaveTextContent(
    "2 activos caducados eliminados",
  );
});
test("a denied delete preserves the capture and reports the permission failure", async () => {
  const deleted = vi.fn();
  mock.request.mockImplementation(async (operation, input) => {
    if (operation === "conversations.list") return [];
    if (operation === "assets.get") return asset;
    throw new Error("PERMISSION_DENIED");
  });
  render(<CaptureAsset id={asset.id} onDeleted={deleted} />);
  fireEvent.click(
    await screen.findByRole("button", { name: "Solicitar borrar captura" }),
  );
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "PERMISSION_DENIED",
  );
  expect(screen.getByRole("img")).toBeInTheDocument();
  expect(deleted).not.toHaveBeenCalled();
});
