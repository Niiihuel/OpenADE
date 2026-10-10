# Validación del editor nativo — 5 de octubre de 2026

## Alcance entregado

El editor usa `openide-editor`, `openide-ui` y el nuevo `openide-language`.
Incluye búsqueda/reemplazo Unicode con opciones y scope, multicursor, selección
rectangular, comandos de líneas, snippets, pares, indentación, finales de línea,
wrap/folding, breadcrumbs navegables, símbolos, menús contextuales y diagnósticos.
Tree-sitter analiza los lenguajes soportados; LSP conecta servidores reales para
completar, documentar, navegar, renombrar, formatear y revisar acciones de código.
Las ediciones multiarquivo se validan y se presentan antes de aplicarse.

Los borradores se guardan fuera del proyecto, se recuperan al reiniciar y mantienen
su versión de disco. Las operaciones de crear, guardar como, recargar, comparar y
recuperar archivos eliminados usan el worker de archivos y la autoridad del journal.
La configuración de indentación, wrap, pares y ejecutables LSP se persiste.

## Pruebas automatizadas

La suite completa del workspace con `agents-window/editor-e2e` pasó **2.376 pruebas**
y dejó **85 pruebas optativas** sin ejecutar en esa corrida. Las pruebas optativas
seleccionadas para este trabajo se ejecutan aparte; otras necesitan servicios,
credenciales o escenarios de sistema que no forman parte de este cambio.

Los tests host atraviesan controles renderizados, entrada del árbol UI, workers,
controladores y archivos temporales reales. Cubren búsqueda, navegación, edición,
settings, context menu, rutas, guardado, conflictos, archivos ausentes, creación,
recuperación y cierre. Las pruebas de componentes añaden IME, Unicode, selección,
coordenadas de wrap/folds, diagnóstico versionado y límites de caché.

También pasaron las dos pruebas con **rust-analyzer real**:

- Transporte stdio: initialize, didOpen, hover, símbolos y cierre.
- Host: conectar desde ajustes, hover, F12, Shift+F12, Ctrl+Space, F2,
  revisar/aplicar renombrado y formato, y Ctrl+S con verificación en disco.

El servidor utilizado fue `rust-analyzer 2026-06-15`, configurado mediante
`OPENIDE_RUST_ANALYZER`; los tests esperan resultados semánticos después de la
carga de Cargo, además del handshake del protocolo.

## Revisión visual y escritorio

El exportador produce 18 variantes con fuentes y métricas nativas:
320, 520 y 900 puntos × toolbar, buscar/reemplazar, guardar como, nuevo archivo,
símbolos y menú contextual. Las comprobaciones geométricas verifican que los
controles no desborden y quede espacio para el código. Se revisaron las imágenes
en `/tmp/openade-editor-workbench-review`.

El harness `scripts/editor-desktop-e2e.py` crea un proyecto temporal y una ventana
nativa en un workspace libre. Cada operación de teclado verifica el PID destino.
La feature `editor-e2e` y el marcador del fixture evitan usar por accidente una
carpeta real. El harness verifica escritura Unicode, buscar/seleccionar/reemplazar,
undo, portapapeles, menú contextual y recuperación después de matar y reiniciar
**su propio proceso**. Los resultados y capturas se registran junto al reporte.

La corrida nativa final **pasó los seis recorridos**. Se verificó el archivo
resultante después de cada operación; el reinicio recuperó el texto sin guardar.
La prueba encontró y permitió corregir la agrupación Undo al reemplazar una
selección mediante varias teclas. El reporte está en
`/tmp/openade-editor-native-e2e/report.json`; las capturas inicial, contextual y
recuperada están en esa misma carpeta. Ambos logs nativos quedaron vacíos.

Clippy estricto pasó para `openide-editor`, `openide-ui` y `openide-language`
incluyendo todos sus targets. La suite del workspace tardó 38,40 segundos,
excluyendo compilación. El build de escritorio mantiene advertencias de código
sin uso en el host; no se presenta como un host libre de todas las advertencias.
`rtk python3 scripts/build.py` completó además el build normal optimizado con
`--locked`, sin la feature de fixtures E2E.

## Rendimiento y límites

Los archivos y el protocolo LSP se procesan fuera del frame. La búsqueda y el
análisis se agrupan, cancelan y etiquetan por documento/revisión. El pool de
análisis y los mapas retenidos de editor/diff están acotados; el servidor inactivo
despierta la UI al publicar resultados. El render sólo genera las líneas visibles.
Los textos de diagnóstico/hover tienen límites de presentación explícitos.

El benchmark de 150.000 líneas mide CPU del componente con fuentes nativas,
100 frames estáticos y 100 ediciones más frame. **No mide toda la ventana, GPU,
latencia de pantalla ni garantiza 120 Hz.** En el perfil `desktop` optimizado, el frame estático midió media **0,237 ms**,
p95 **0,252 ms** y máximo **0,267 ms**; edición más frame, media **0,273 ms**,
p95 **0,295 ms** y máximo **0,302 ms**. Es una medición local del componente con
texto plano, wrap desactivado y sin servidor; no cubre todos los escenarios.

No se afirma paridad total con Zed: colaboración/CRDT, Vim, inlay hints, gestión de
extensiones de lenguajes y el conjunto completo del protocolo LSP no
forman parte de este editor. Los límites de documento, análisis, historial y
snippets están en [el diseño](native-code-editor-design.md).
El depurador DAP incorporado posteriormente y sus límites específicos se
documentan en [su validación](native-debugger-validation.md).

## Reproducción

### Revisión posterior: menús, hover, iconografía y navegador

Validación del 5 de octubre de 2026 tras la integración de Material Icon Theme
5.38.1 y los menús contextuales de 288 px: **2.417 pruebas pasadas y 88 opt-in
ignoradas**, en 28 suites del workspace (40,10 s, sin compilación). Clippy estricto
de `openide-ui` con todos sus targets pasó sin advertencias.

Se ejecutaron aparte los tres exporters de menús e iconos y se inspeccionaron
sus imágenes: menús en 360×280, 520×640 y 1100×760, final de scroll, catálogo de
165 iconos en claro/oscuro y uso en Files, Git y editor. Las regresiones raster
verifican que los SVG respeten los límites de su caja a escalas 1×/2×.

Se volvieron a ejecutar los **seis flujos E2E del editor en Wayland** con teclado
y portapapeles reales, incluyendo recuperación tras cerrar la fixture. La
captura inicial confirma Rust en la pestaña y el breadcrumb. El navegador pasó
**cuatro casos reales del bridge** y **cinco comprobaciones desde la ventana
nativa**, con escritura rápida y portapapeles; detalles en
[Navegador](browser-runtime.md). Estas pruebas no son una medición global de FPS.

Los artefactos locales están en `/tmp/openade-compact-context-review`,
`/tmp/openade-file-icon-review`, `/tmp/openade-editor-native-e2e` y
`/tmp/openade-browser-native-e2e`.

### Comandos

```sh
rtk cargo test --workspace --features agents-window/editor-e2e
rtk cargo clippy -p openide-editor -p openide-ui -p openide-language --all-targets -- -D warnings
rtk cargo test -p openide-language -p agents-window real_rust_analyzer -- --ignored --nocapture
rtk cargo test -p agents-window export_editor_workbench_visual_review -- --ignored --nocapture
rtk cargo test --profile desktop -p agents-window benchmark_code_editor_visible_frames_and_typing -- --ignored --nocapture
rtk cargo build --profile desktop -p agents-window --features editor-e2e
rtk python3 scripts/editor-desktop-e2e.py --binary target/desktop/agents-window --output /tmp/openade-editor-native-e2e
rtk python3 scripts/build.py
```

En NixOS, ejecutar dentro del shell de desarrollo o proporcionar `cc`, `pkg-config`
y las bibliotecas Wayland/xkbcommon/Vulkan/OpenGL. El test de escritorio requiere
Hyprland, `wtype` y `grim`; el test semántico requiere un ejecutable real de
rust-analyzer y el toolchain de Rust.

### Estructura y selección: escritorio privado, 6 de octubre de 2026

La suite `cargo test --workspace --features editor-e2e` terminó con **2592
pruebas aprobadas y 94 optativas ignoradas** en 28 suites. Después del ajuste
acotado para no inventar guías en prefijos de 512 o más espacios, se repitieron
las cinco pruebas de guías y pasaron. Compilaron tanto el fixture con
`editor-e2e` como el ejecutable normal con `--profile desktop` (sin errores;
21 avisos existentes de imports y código sin uso). La sesión de escritorio del
usuario y sus PTY se conservaron abiertas; el nuevo ejecutable se usa en el
próximo arranque.

`scripts/editor-structure-desktop-e2e.py` pasó **cinco recorridos nativos** con
el binario de fixture `/tmp/openide-editor-structure-e2e-bin`, una fuente Rust
temporal de 168 líneas y sin proveedor ni depurador configurado:

- Selección real mediante 18 movimientos Shift+Down, que abarca 19 filas físicas
  con extremos parciales e incluye una línea vacía.
  Los offsets de inicio/fin permanecieron exactos al reducir la ventana a
  900×640 y desplazar el editor; texto, revisión y archivo en disco no cambiaron.
- Plegar y desplegar desde el chevron modificó las filas visibles sin editar el
  documento ni conservar regiones interactivas del cuerpo oculto.
- F9 y clic en el gutter añadieron/quitaron un marcador local pendiente. Su
  tooltip indicó que no hay depurador conectado y que no detiene la ejecución.
- El scroll mostró tres encabezados de scopes reales; pulsar el de la función
  llevó el cursor a su declaración sin modificar el contenido.
- Una edición invalidó los marcadores y los IDs de controles de la revisión
  anterior. Reemplazar el buffer por sintaxis incompleta retiró los folds y
  scopes obsoletos y conservó el archivo original sin guardar esos cambios.

El reporte está en `/tmp/openide-editor-structure-native/report.json` y las ocho
capturas finales en esa carpeta. Se inspeccionaron el contorno continuo de la
selección en ancho normal y estrecho, el clip al desplazar, los scopes fijados,
el bloque plegado y el estado después de invalidar el análisis. El log nativo
quedó vacío. La primera corrida sólo requirió corregir la aserción del texto del
tooltip para reconocer el salto de línea de su descripción; el mensaje ya se
mostraba completo y no se cambió producción por ese fallo de prueba.

La ventana usó `eframe/wgpu` sobre Wayland, en un KWin virtual y un Hyprland
anidado privados. Teclado y puntero virtual se dirigieron exclusivamente a ese
runtime; cada entrada verificó el PID y el script rechaza el escritorio habitual.
El observador de la feature `editor-e2e` sólo lee estados reales, con texto
limitado a 32 KiB y arrays de estructura acotados. No inyecta selección, folds o
marcadores. Estas verificaciones no miden FPS ni validan un adaptador DAP.

Después de integrar el depurador real se repitieron los cinco recorridos con
`/tmp/openide-debug-e2e-bin` y pasaron de nuevo. El gutter conserva la selección,
el plegado y los controles de estructura. Sin configuración, F9 y clic producen
estado `Pending`, ninguna línea de ejecución y sesión `Unconfigured`; el tooltip
actual explica que el adaptador todavía no confirmó el punto. Sólo se actualizó
esa expectativa de texto del script y se añadieron las comprobaciones explícitas
de estado. El reporte y las ocho capturas de la carpeta anterior corresponden a
esta última corrida; el log nativo quedó vacío. Los cuatro recorridos DAP reales
y las cifras de pruebas automatizadas actuales se registran por separado en
[Validación del depurador nativo](native-debugger-validation.md).

Reproducción, con el binario ya compilado y bibliotecas nativas disponibles:

```sh
rtk python3 scripts/headless-desktop-fixture.py \
  --kwin /absolute/path/to/kwin_wayland \
  --output /tmp/openide-editor-structure-private \
  -- python3 scripts/editor-structure-desktop-e2e.py \
  --binary /absolute/path/to/agents-window-with-editor-e2e \
  --wtype /absolute/path/to/wtype \
  --output /tmp/openide-editor-structure-native
```
