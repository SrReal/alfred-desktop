# AlfredPet Community

Asistente de escritorio para Windows 11 x64 con chat de Claude, memoria local,
herramientas y autorizaciones revisables. El frontend utiliza React y Tauri 2;
el núcleo de Rust comprueba los permisos antes de ejecutar acciones.

**Edición pública de desarrollo.** Incluye un marcador geométrico original con
pulso y distintivos de estado. El personaje definitivo sigue pendiente de
revisión; este repositorio no ofrece todavía un instalador público oficial.

![Marcador de desarrollo original](assets/public/pet-reference.png)

## Capacidades

- Chat con Markdown, enlaces, imágenes, adjuntos, historial, edición y variantes.
- Modelos compatibles con la suscripción de Claude Code y personalidad cortés.
- Contexto de pantalla autorizado, selección de texto y dictado local revisable.
- Memoria local, búsqueda, tareas, recordatorios, comandos, macros y rutinas.
- Archivos, ventanas, terminal, navegador y herramientas administrativas revisadas.
- Conexiones GitHub, Notion, Drive, REST, bases de datos, Home Assistant y MCP.
- Skills y plugins con revisión, pruebas, activación y permisos propios.
- Copia y recuperación del perfil y auditoría de acciones.

Los 26 módulos tienen límites y requisitos propios. Consulta el
[manual completo](output/pdf/AlfredPet-Manual-de-uso.pdf). El acceso de Claude usa
el CLI oficial y una suscripción admitida; la aplicación no incluye una cuenta
ni garantiza acceso a cada modelo o proveedor externo.

## Desarrollo

Necesitas Windows 11 x64, Node **v24.21.0**, Rust con MSVC, Visual Studio Build
Tools, WebView2 y Python con `reportlab`, `pypdf` y `Pillow`.

```powershell
npm ci --ignore-scripts
node scripts/prepare.mjs
npm run tauri -- dev
```

La versión y huella del runtime Node están fijadas en `scripts/prepare.mjs`.
Los modelos semánticos son opcionales y se descargan bajo consentimiento; sus
pesos no se incluyen en Git. No cambies el runtime sin actualizar sus hashes y
avisos de licencia.

## Comprobaciones

```powershell
npm run typecheck
npm run lint
npm test
node --test tests/vscode-bridge.test.mjs
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib --tests
python docs/manual/build_manual.py --check
node docs/manual/check_bundle.mjs
python tools/check_publication.py --history
```

Las comprobaciones automáticas no acreditan por sí solas micrófono, UAC,
selectores, pantallas físicas, consumo prolongado ni cuentas externas reales.
Los pendientes de uso están identificados en el manual.

## Datos y permisos

La edición pública utiliza `%LOCALAPPDATA%\AlfredPetPublic` y un identificador
de aplicación propio. Los secretos de las conexiones se guardan mediante
Windows Credential Manager; el CLI oficial gestiona su autenticación. Revisa
las acciones sensibles y los permisos recordados. Una autorización no garantiza
que un servicio externo vaya a aceptar la operación.

## Contribuir y licencia

El repositorio recibe snapshots verificados del núcleo común. Puedes proponer
correcciones mediante issues o pull requests; los cambios se revisan e integran
en el núcleo antes de sincronizarlos. No se importan datos ni historiales de
desarrollo personales.

Código propio y recursos geométricos originales bajo [MIT](LICENSE), copyright
2026 SrReal. Las dependencias conservan sus [avisos](assets/licenses/README.md)
y condiciones. Consulta [LICENSING.md](LICENSING.md).
