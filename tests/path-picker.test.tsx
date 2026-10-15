import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { useState } from "react";
import { beforeEach, expect, test, vi } from "vitest";
import { request } from "../src/api";
import { PathPicker, PathsPicker } from "../src/ui/PathPicker";
import {
  ConnectionConfig,
  JsonPathEditor,
  toolPathFields,
} from "../src/ui/JsonPathEditor";
vi.mock("../src/api", () => ({ request: vi.fn() }));
const call = vi.mocked(request);
beforeEach(() => {
  call.mockReset();
});

test("selects a Unicode file, cancellation preserves it and removal never executes a tool", async () => {
  function Fixture() {
    const [path, setPath] = useState("");
    return (
      <PathPicker label="Archivo" mode="file" value={path} onChange={setPath} />
    );
  }
  call
    .mockResolvedValueOnce(["D:\\Mis archivos\\señor.png"])
    .mockResolvedValueOnce(null);
  render(<Fixture />);
  fireEvent.click(screen.getByRole("button", { name: "Archivo" }));
  await screen.findByText("señor.png");
  expect(call).toHaveBeenCalledWith("paths.pick", {
    mode: "file",
    title: "Archivo",
    multiple: false,
    extensions: [],
    default_name: "",
  });
  fireEvent.click(screen.getByLabelText("Archivo"));
  await waitFor(() => expect(screen.getByLabelText("Archivo")).toBeEnabled());
  expect(screen.getByLabelText("Archivo")).toHaveAttribute(
    "title",
    "D:\\Mis archivos\\señor.png",
  );
  fireEvent.click(screen.getByLabelText("Quitar Archivo"));
  expect(screen.getByLabelText("Archivo")).toHaveAttribute("title", "Archivo");
  expect(
    call.mock.calls.every(([operation]) => operation === "paths.pick"),
  ).toBe(true);
});

test("folder selection appends distinct roots and allows individual removal", async () => {
  function Fixture() {
    const [paths, setPaths] = useState(["D:\\Uno"]);
    return (
      <PathsPicker
        label="Carpetas"
        mode="folder"
        multiple
        append
        value={paths}
        onChange={setPaths}
      />
    );
  }
  call.mockResolvedValue(["D:\\Uno", "D:\\Dos"]);
  render(<Fixture />);
  fireEvent.click(screen.getByLabelText("Carpetas"));
  await screen.findByText("Dos");
  expect(screen.getAllByText("Uno")).toHaveLength(1);
  expect(call).toHaveBeenCalledWith(
    "paths.pick",
    expect.objectContaining({ mode: "folder", multiple: true }),
  );
  fireEvent.click(screen.getByLabelText("Quitar Uno"));
  expect(screen.queryByText("Uno")).not.toBeInTheDocument();
});

test("an open dialog cannot double-dispatch or change a different form after its mode changes", async () => {
  let finish!: (paths: string[]) => void;
  call.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const change = vi.fn();
  const { rerender } = render(
    <PathPicker label="Destino" mode="file" value="" onChange={change} />,
  );
  fireEvent.click(screen.getByLabelText("Destino"));
  fireEvent.click(screen.getByLabelText("Destino"));
  expect(call).toHaveBeenCalledTimes(1);
  rerender(
    <PathPicker label="Destino" mode="save" value="" onChange={change} />,
  );
  await act(async () => finish(["D:\\antiguo.txt"]));
  expect(change).not.toHaveBeenCalled();
  expect(screen.getByLabelText("Destino")).toBeEnabled();
});

test("shows picker errors without losing the previous selection", async () => {
  const change = vi.fn();
  call.mockRejectedValueOnce(new Error("No se puede abrir el selector"));
  render(
    <PathPicker
      label="Archivo"
      mode="file"
      value="D:\\original.txt"
      onChange={change}
    />,
  );
  fireEvent.click(screen.getByLabelText("Archivo"));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "No se puede abrir el selector",
  );
  expect(change).not.toHaveBeenCalled();
  expect(screen.getByText("original.txt")).toBeInTheDocument();
});

test("new destinations use Save and reject an invalid multiple result", async () => {
  call.mockResolvedValue(["D:\\one.txt", "D:\\two.txt"]);
  const change = vi.fn();
  render(
    <PathPicker
      label="Resultado"
      mode="save"
      defaultName="resultado.txt"
      value=""
      onChange={change}
    />,
  );
  fireEvent.click(screen.getByLabelText("Resultado"));
  await screen.findByRole("alert");
  expect(change).not.toHaveBeenCalled();
  expect(call).toHaveBeenCalledWith(
    "paths.pick",
    expect.objectContaining({
      mode: "save",
      default_name: "resultado.txt",
      multiple: false,
    }),
  );
});

test("connection paths use selectors and survive an incomplete JSON edit", async () => {
  function Fixture() {
    const [value, setValue] = useState(
      JSON.stringify({
        engine: "sqlite",
        path: "D:\\original.db",
        read_only: true,
      }),
    );
    return (
      <>
        <ConnectionConfig
          module="database"
          value={value}
          onChange={setValue}
          disabled={false}
        />
        <output data-testid="config">{value}</output>
      </>
    );
  }
  render(<Fixture />);
  const editor = screen.getByLabelText("Configuración de conexión");
  expect(editor).not.toHaveValue(expect.stringContaining("original.db"));
  fireEvent.change(editor, { target: { value: "{" } });
  expect(screen.getByLabelText("Archivo de base de datos")).toBeDisabled();
  fireEvent.change(editor, {
    target: { value: '{"engine":"sqlite","read_only":false}' },
  });
  expect(JSON.parse(screen.getByTestId("config").textContent!)).toEqual({
    engine: "sqlite",
    read_only: false,
    path: "D:\\original.db",
  });
  call.mockResolvedValueOnce(["D:\\nueva.db"]);
  fireEvent.click(screen.getByLabelText("Archivo de base de datos"));
  await screen.findByText("nueva.db");
  expect(JSON.parse(screen.getByTestId("config").textContent!).path).toBe(
    "D:\\nueva.db",
  );
});

test("tool paths are separated from ordinary arguments without altering their values", async () => {
  function Fixture() {
    const [value, setValue] = useState(
      '{"path":"D:\\\\old.pdf","destination":"","page":2}',
    );
    return (
      <>
        <JsonPathEditor
          label="Opciones"
          value={value}
          onChange={setValue}
          fields={toolPathFields("documents.split", [
            "path",
            "destination",
            "page",
          ])}
        />
        <output data-testid="args">{value}</output>
      </>
    );
  }
  call.mockResolvedValueOnce(["D:\\split.pdf"]);
  render(<Fixture />);
  expect(
    JSON.parse(
      (screen.getByLabelText("Opciones") as HTMLTextAreaElement).value,
    ),
  ).toEqual({ page: 2 });
  fireEvent.click(screen.getByLabelText("Guardar resultado como"));
  await screen.findByText("split.pdf");
  expect(JSON.parse(screen.getByTestId("args").textContent!)).toEqual({
    path: "D:\\old.pdf",
    destination: "D:\\split.pdf",
    page: 2,
  });
});

test("registry keys, remote paths and monitoring identifiers remain ordinary parameters", () => {
  expect(
    toolPathFields("drive.upload", ["connection", "action"], {
      action: "start",
    }),
  ).toEqual([
    { key: "path", label: "Archivo para subir a Drive", mode: "file" },
  ]);
  expect(
    toolPathFields("drive.upload", ["connection", "action"], {
      action: "status",
    }),
  ).toEqual([]);
  expect(toolPathFields("drive.download", ["connection", "file_id"])).toEqual(
    [],
  );
  for (const tool of [
    "windows.registry_get",
    "rest.request",
    "mcp.read_resource",
    "monitoring.create",
  ])
    expect(toolPathFields(tool, ["path", "destination"])).toEqual([]);
  expect(
    toolPathFields("monitoring.create", ["kind", "path"], { kind: "folder" })[0]
      .mode,
  ).toBe("folder");
  expect(
    toolPathFields("images.compare", ["path", "destination"])[1].mode,
  ).toBe("file");
});
