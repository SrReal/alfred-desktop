# AlfredPet Community

[![Descargar AlfredPet Community 0.1.0 para Windows x64](https://img.shields.io/badge/Descargar_instalador-Windows_x64-2ea44f?style=for-the-badge)](https://github.com/SrReal/alfred-desktop/releases/download/v0.1.0/AlfredPet-Community_0.1.0_x64-setup.exe)

Versión preliminar **0.1.0** para Windows 11 x64. Instalador sin firma digital.
[Notas de versión, manual y SHA-256](https://github.com/SrReal/alfred-desktop/releases/tag/v0.1.0).

Asistente de escritorio para Windows 11 x64 con chat de Claude, memoria local,
herramientas y autorizaciones revisables. El frontend utiliza React y Tauri 2;
el núcleo de Rust comprueba los permisos antes de ejecutar acciones.

**Edición pública de desarrollo.** El personaje original de PixelLab ya está
integrado en la mascota, los avatares y los iconos de esta edición:

<img src="assets/character-preview/pet-idle.png" alt="Personaje original generado con PixelLab para AlfredPet Community" width="288" height="384">

La imagen conserva los píxeles originales de PixelLab; su
[procedencia y condiciones](assets/character-preview/NOTICE.md) están incluidas.
La mascota utiliza 16 animaciones con 120 fotogramas a 144 x 192 px: reposo,
saludo, arrastre, caída, pensamiento, trabajo, búsqueda, observación, uso del
equipo, esperas, éxito, error, privacidad, no molestar y descanso. La app y la
bandeja muestran su cara; el chat utiliza su retrato y una brújula para el usuario.
Puedes abrir [la galería local](docs/character-preview.html) después de clonar
el repositorio para revisar los estados y tamaños. El instalador público de
desarrollo tiene ejecutable y perfil propios. Descárgalo con el botón superior
o consulta [todas las versiones publicadas](https://github.com/SrReal/alfred-desktop/releases).

## Capacidades

- Chat con Markdown, enlaces, imágenes, adjuntos, historial, edición y variantes.
- Modelos compatibles con la suscripción de Claude Code y personalidad cortés.
- Contexto de pantalla autorizado, selección de texto y dictado local revisable.
- Memoria local, búsqueda, tareas, recordatorios, comandos, macros y rutinas.
- Archivos, ventanas, terminal, navegador y herramientas administrativas revisadas.
- Conexiones GitHub, Notion, Drive, REST, bases de datos, Home Assistant y MCP.
- Skills y plugins con revisión, pruebas, activación y permisos propios.
- Copia y recuperación del perfil y auditoría de acciones.

El instalador utiliza `AlfredPet-Community.exe`, el nombre AlfredPet Community
y el perfil `%LOCALAPPDATA%\AlfredPetPublic`. Rechaza una carpeta que contenga
`AlfredPet.exe`. Las credenciales nativas usan el prefijo `alfredpet.public.`.
Los atajos públicos son Ctrl+Alt+Mayús+Espacio (comandos), Ctrl+Alt+U (texto)
y Ctrl+Alt+Mayús+U (región).

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
# Instalador Windows x64:
npm run tauri -- build --bundles nsis -- --locked
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
python tools/build_character.py --check
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

El instalador incluye [avisos de distribución](assets/licenses/AlfredPet-Community-Distribution.txt),
notices de dependencias Rust/web y fuentes originales de las dependencias MPL.

Código propio y recursos geométricos originales bajo [MIT](LICENSE), copyright
2026 SrReal. El personaje, sus animaciones e iconos derivados de PixelLab tienen
[condiciones propias](assets/character-source/NOTICE.md) y están excluidos de esa
concesión MIT. Las dependencias conservan sus [avisos](assets/licenses/README.md)
y condiciones. Consulta [LICENSING.md](LICENSING.md).
