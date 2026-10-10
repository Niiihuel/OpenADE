# Servicios de lenguaje nativos

`openide-language` contiene el parser y el cliente LSP, sin depender del renderer.
Importar el crate o construir una caché no inicia procesos ni hilos. Las solicitudes
se hacen sobre `BufferSnapshot`: la identidad del documento y su revisión acompañan
cada resultado. El host debe comprobar ambas antes de aplicar cambios.

## Parser y resaltado

`ParserEngine::analyze` usa gramáticas reales de Tree-sitter para Rust, JavaScript,
TypeScript, TSX, Python, JSON, TOML, CSS y HTML. TypeScript y TSX usan gramáticas
distintas para conservar la semántica de las aserciones `<Tipo>valor`. Usa las consultas de resaltado de
esas gramáticas y recorre el árbol para extraer símbolos y regiones plegables.
Los rangos de resaltado y símbolos son offsets de caracteres Unicode del editor;
`Fold::rows` es un intervalo de filas físicas `[start, end)` cuya primera fila queda
visible. Los errores de sintaxis y los resultados incompletos son estados distintos.

Markdown utiliza el parser CommonMark/GFM de `pulldown-cmark`, con tablas, tareas,
tachado y notas al pie habilitados. Reconoce `md`, `markdown`, `mdown`, `mkd`,
`mkdn` y `mdwn`; el modo también puede elegirse desde el footer. Produce los mismos
rangos Unicode versionados que los demás lenguajes para encabezados, énfasis,
enlaces, código, tablas, marcadores y HTML literal. Los spans superpuestos se
resuelven por prioridad: por ejemplo, el código inline mantiene su categoría
dentro de un encabezado. El HTML se resalta como fuente; no se ejecuta.

Los encabezados ATX y Setext generan símbolos y secciones plegables; sus rangos
abarcan el cuerpo hasta el siguiente encabezado de nivel igual o superior para
permitir breadcrumbs y navegación por símbolos. Listas, citas y bloques de código
también producen pliegues cuando ocupan varias filas. Los bloques cercados con
lenguaje conocido reutilizan el parser correspondiente y remapean sus offsets al
documento Markdown. Si las marcas de una lista o cita impiden esa correspondencia
directa, el bloque conserva resaltado literal. Los lenguajes desconocidos no
ejecutan herramientas ni intentan instalar una gramática.

Cada parser conserva como máximo cuatro árboles. Para una nueva revisión del mismo
documento calcula el cambio entre snapshots, actualiza el árbol anterior y solicita
un parse incremental. Cambiar de documento o lenguaje evita reutilizar un árbol ajeno.
El análisis Markdown recompone sus eventos por snapshot; no afirma conservar un
árbol incremental de Tree-sitter. Comparte cancelación, límites de entrada y la
identidad de documento/revisión de los resultados.
El presupuesto predeterminado es 8 MiB por documento, 256 KiB por línea, 262.144
capturas, 4.096 símbolos, 16.384 regiones plegables, 524.288 nodos y 250 ms de parse/consulta. Una cancelación o límite devuelve
`truncated`; no inventa un parse completo. Las líneas gigantes se detectan recorriendo
chunks prestados antes de crear una copia del texto.

`AnalysisWorker` ofrece dos workers globales, creados al enviar la primera solicitud,
y una cola de 64 clientes. Cada cliente conserva sólo su último snapshot y resultado.
`submit` devuelve `false` si no puede admitir más trabajo; el host puede reintentar.
Un nuevo snapshot cancela el anterior, también durante el parse y las consultas.

El resaltado autónomo de `CodeEditor`/`CodeDiff` usa `SyntaxCache`, con dos workers
globales y una cola de ocho trabajos. Los buffers de código completos usan Tree-sitter; los
fragmentos de diff usan un lexer acotado con estados independientes de la versión
anterior y posterior. Un diff no dispone del archivo completo para reconstruir su
árbol con precisión. Las cachés vacías y el lenguaje Plain no crean un hilo por vista.
Cuando el host suministra resaltado externo vigente puede evitar el análisis autónomo.

En Markdown, la caché de buffers completos usa el análisis CommonMark/GFM; los
fragmentos de diff mantienen un estado propio de cercado para cada lado. Una
revisión nueva invalida los scopes anteriores antes de mostrar los resultados.
El minimapa consume filas visuales y resaltado vigente del editor con muestreo
acotado; no inicia otro servidor ni modifica los rangos del parser.

## Protocolo LSP

`LanguageServer::spawn(ServerConfig)` arranca un ejecutable explícito mediante stdio.
El host espera `Event::Initialized` y después llama `open`, `change`, `request` y
`close`. La API implementa completion, hover, definition, references, rename,
formatting, code actions y document symbols. Respeta las capabilities negociadas y
rechaza funciones que el servidor no anuncia. Se negocian posiciones UTF-16;
`offset_to_utf16` y `utf16_to_offset` comprueban límites y no dividen pares sustitutos.
El índice de coordenadas reconoce CR, LF y CRLF como el protocolo; los separadores
Unicode que Ropey representa como filas del editor no alteran las filas LSP. La caché
LRU conserva cuatro índices con un máximo de un millón de líneas por snapshot. El
worker de escritura precalienta el índice; las consultas posteriores usan búsqueda
binaria y las métricas UTF-16 del rope.

Los mensajes completos de apertura y cambios se construyen y serializan en el worker
de escritura a partir de snapshots baratos. El cliente usa cambios completos, válidos
también para sincronización incremental del protocolo. Esto conserva la corrección
tras undo, reemplazos y transacciones múltiples sin reconstruir deltas en la UI.

El transporte tiene una cola de escritura de 16 mensajes/16 MiB, ocho eventos de
salida, 64 solicitudes pendientes y 32 documentos abiertos. Rechaza headers mayores
de 8 KiB, respuestas mayores de 4 MiB y mensajes salientes mayores de 16 MiB. Drena
stderr sin retener logs sin límite. Initialize dispone de 15 segundos y cada petición
de 30 segundos; el host debe sondear `try_recv` para recibir resultados y timeouts.

Los cambios, cierres y cancelaciones invalidan inmediatamente las respuestas locales
y envían `$/cancelRequest`. La versión LSP crece incluso al reabrir una ruta para que
un diagnóstico antiguo no se atribuya a un nuevo buffer con la misma URI. Los eventos
se validan al recibirlos y otra vez al consumirlos. Los diagnósticos sin versión se
entregan como `UnversionedDiagnostics`; no se etiquetan como pertenecientes al buffer
actual. Los cambios solicitados por el servidor con `workspace/applyEdit` se rechazan:
el host debe presentar y autorizar las ediciones devueltas por las acciones del usuario.
`text_edits` convierte listas a transacciones del editor y rechaza rangos inválidos o
solapados. No aplica los cambios por sí mismo.

`server_candidates` busca overrides `OPENIDE_*_LANGUAGE_SERVER` y PATH; Rust permite
`OPENIDE_RUST_ANALYZER`. Una ruta encontrada es sólo candidata. `probe` verifica una
comprobación de versión con timeout; la disponibilidad efectiva se confirma durante
initialize. `stdio_args` conoce los switches de los servidores habituales. Nada instala
servidores automáticamente. Un shim de rustup sin el componente instalado no constituye
un servidor disponible. `set_waker` permite avisar al host cuando llegan eventos; un
servidor conectado e inactivo no obliga a repintar continuamente.

El host conserva overrides de ejecutable por proyecto y lenguaje durante la sesión.
El control LSP del footer abre un popover con el lenguaje, el estado actual y un
campo de ejecutable opcional. Conectar o Reiniciar usan el mismo arranque
asíncrono; la ruta es un ejecutable, no un comando de shell. Vaciarla vuelve a la
detección automática. La elección se conserva por proyecto y lenguaje. El
popover no reserva espacio sobre el editor y ofrece acceso a Problemas en el
dock inferior. Sólo se inicia una conexión a la vez y las
conexiones obsoletas se cancelan. Los cambios de varios archivos se cargan sin cambiar
la pestaña activa, se revisan y se aplican bajo locks después de validar origen,
versiones y límites de todos los buffers. Permanecen sin guardar y se pueden deshacer.
Las acciones de código que sólo contienen comandos externos no se ejecutan; se muestra
que no contienen ediciones aplicables. Los diagnósticos versionados alimentan el gutter
y los subrayados, mientras los diagnósticos sin versión permanecen en «Problemas».

El footer del IDE separa errores, advertencias y el resto de severidades en su
resumen y tooltip, e indica cuándo los diagnósticos no corresponden a la revisión
actual. Sus acciones de lenguaje, sangría y finales de línea validan proyecto,
pestaña, identidad del buffer y revisión antes de aplicarse. Elegir LF, CRLF o CR
usa una transacción real del editor que admite deshacer; cambiar sangría ajusta
las preferencias para futuras ediciones. El indicador UTF-8 sólo describe la
codificación que valida el cargador y usa el guardado. Markdown no descubre ni
arranca automáticamente un servidor LSP por agregar el modo de resaltado.

## Validación y referencias

Las pruebas incluyen las gramáticas habilitadas, Unicode/CRLF/UTF-16, revisiones incrementales,
cancelación, límites, y un proceso Python que habla JSON-RPC real por stdin/stdout.
Las pruebas de Markdown cubren scopes inline y multilínea, referencias, Unicode,
cercados con lenguajes conocidos y desconocidos, símbolos de secciones, pliegues,
ediciones y cancelación. Las pruebas del footer comprueban geometría nativa en
anchos compactos, acciones de menús, conversiones de finales de línea con undo y
rechazo de acciones de un documento o revisión anteriores.
La prueba explícita `real_rust_analyzer_initialize_open_hover_symbols_and_close` usa el
binario indicado por `OPENIDE_RUST_ANALYZER`; es independiente del fixture. La prueba
host `real_rust_analyzer_host_toolbar_keyboard_navigation_rename_format_and_save`
atraviesa controles, atajos, respuestas reales, revisión de cambios, aplicación y
guardado con Ctrl+S en un proyecto temporal.

La implementación es original. Se consultaron la [API de Tree-sitter](https://docs.rs/tree-sitter/0.25.10/tree_sitter/),
las gramáticas distribuidas por sus autores y la [especificación LSP 3.17](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/).
Las referencias locales de Zed (`crates/language/src/syntax_map.rs`,
`crates/language/src/buffer.rs`, `crates/project/src/lsp_store.rs`) sirvieron para
contrastar separación de servicios, revisión de snapshots y cancelación; no se copió
su implementación.
