# AlfredPet Community

Asistente de escritorio para Windows 11 x64 con chat de Claude, memoria local,
herramientas y autorizaciones revisables. El frontend utiliza React y Tauri 2;
el núcleo de Rust comprueba los permisos antes de ejecutar acciones.

**Edición pública de desarrollo.** Esta es la vista previa del personaje original
generado con PixelLab para la edición pública:

<img src="assets/character-preview/pet-idle.png" alt="Personaje original generado con PixelLab para AlfredPet Community" width="288" height="384">

La imagen conserva los píxeles originales de PixelLab; su
[procedencia y condiciones](assets/character-preview/NOTICE.md) están incluidas.
La integración del personaje en las 16 animaciones, avatares e iconos sigue
pendiente. La aplicación de desarrollo utiliza todavía el marcador geométrico
con pulso y distintivos de estado. Este repositorio no ofrece un instalador
público oficial.

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
2026 SrReal. La imagen de PixelLab tiene sus
[condiciones propias](assets/character-preview/NOTICE.md) y está excluida de esa
concesión MIT. Las dependencias conservan sus [avisos](assets/licenses/README.md)
y condiciones. Consulta [LICENSING.md](LICENSING.md).
