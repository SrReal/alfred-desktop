# Avisos de runtimes incluidos

El runtime Node incluido se fija a v24.21.0 y al hash SHA-256 comprobado por `scripts/prepare.mjs`. Su licencia completa, incluidos avisos de terceros, procede de `LICENSE` de esa instalación y se conserva en `Node-v24.21.0-LICENSE.txt`. El preparador también la copia a `runtime/LICENSE-node.txt` dentro del instalador.

Playwright Core se reproduce desde `package-lock.json`; su directorio distribuido conserva `LICENSE`, `NOTICE` y `ThirdPartyNotices.txt` bajo `sidecars/browser-host/node_modules/playwright-core/`. No se empaqueta Edge/Chrome ni un navegador descargado. Su versión y el hash del runtime se registran en `docs/evidence/browser-runtime.json`.

La memoria semántica incluye Tokenizers.js 0.2.0 (Apache 2.0), ONNX Runtime Node 1.30.0 (MIT y avisos de terceros), DirectML 1.15.4 (términos del binario Microsoft, licencia de código y avisos del NuGet oficial). ONNX Runtime importa DirectML.dll incluso al seleccionar CPU; se conserva esa dependencia y sus avisos. El proceso distribuido usa sólo CPU y excluye los compiladores GPU dxcompiler/dxil. Las versiones y hashes de los binarios Windows x64 están fijados en `assets/models/semantic-runtime.json` y comprobados por el preparador. Transformers.js 4.3.0 es sólo una referencia de pruebas y no se incluye en el proceso distribuido.

El modelo `sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2` está declarado Apache 2.0 por su autor. Sus pesos no se incluyen en Git ni en el instalador: la aplicación los descarga sólo bajo opt-in, fija la revisión y verifica cada archivo mediante tamaño y SHA-256. El model card del autor también se descarga y verifica. La licencia Apache completa se incluye con Tokenizers.js. Fuentes: [model card](https://huggingface.co/sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2), [ONNX Runtime 1.30.0](https://github.com/microsoft/onnxruntime/tree/v1.30.0), [DirectML 1.15.4](https://www.nuget.org/packages/Microsoft.AI.DirectML/1.15.4).


El código propio y el pack geométrico original se ofrecen bajo MIT. Cada dependencia conserva los términos de su proveedor; estos avisos no convierten sus componentes en MIT ni acreditan una auditoría legal de todos los binarios posibles.
