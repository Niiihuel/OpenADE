# Contexto y uso observado en la UI nativa

`NativeContextUiState` se reconstruye al cambiar selección/ruta, al recibir una
respuesta completa, después de compactar y al abrir el panel Contexto. La app
refresca también tras guardar endpoint, esfuerzo y búsqueda web alojada; un draft
de endpoint inválido que falla al guardar elimina el uso atribuido en pantalla.
Los polls de catálogo y modelos reproyectan los límites/capacidades recibidos.
Estas preferencias cambian el perfil próximo, sin reatribuir respuestas previas.
La app lee el store del dueño seleccionado y valida replay con su store de checkpoints.
Comprueba sesión nativa, proyecto canónico, dueño de cada evento y final exacto
del journal. Una CLI, proyecto ajeno, journal o checkpoint inválido no aporta uso
al medidor. La consulta no admite turnos, modifica journals ni conecta MCP.

## Uso y procedencia

El medidor sólo usa la última `ModelResponseCommitted` si su
`ModelRouteCommitted` coincide exactamente con proveedor, ID de modelo, digest
del endpoint y perfil de petición del backend resuelto. No atribuye uso por
prefijos de modelo ni por coincidencia de nombres entre proveedores. Una ruta
anterior o journal sin el marcador nuevo conserva sus conteos como datos
históricos explícitos, pero no ocupa el medidor de la ruta seleccionada. Los
digests no guardan URL cruda ni credenciales; el perfil liga el protocolo y los
campos de generación, incluido un límite de salida configurado.

Entrada, salida y total son los números reportados por el proveedor. Cache y
razonamiento son subconteos y no se suman otra vez al total. Una última respuesta
sin uso reportado permanece desconocida; no se sustituye con una respuesta más
antigua. Los acumulados incluyen sólo respuestas cuya ruta coincide y muestran
cuántas tienen uso, cuántas no lo reportan y cuántas quedan excluidas por ruta o
procedencia. La cobertura de cache y razonamiento se informa por separado:
campos ausentes son desconocidos y una suma parcial no presupone cero.

Los acumulados son consumo **registrado en respuestas del journal**, no ocupación
actual de la ventana ni un total de facturación. La inferencia de una compactación
explícita no crea `ModelResponseCommitted` en ese journal; su uso no se inventa ni
se incluye en estos acumulados. El cálculo detecta overflow y muestra un error
en vez de una suma truncada.

El porcentaje restante compara el total reportado de la última respuesta con
la ventana del modelo exacto y redondea a un entero, sin baseline fijo. No predice
el tamaño del siguiente pedido. Si sólo se declara un máximo de entrada, el
porcentaje compara entrada contra ese máximo, sin cargarle salida ni llamar
ventana al dato. Checkpoints conservan ese límite independiente. Ventana,
máximo de entrada, máximo de salida declarado y reserva de
salida configurada conservan su significado separado; una entrada parcial del
catálogo no completa límites con valores de otro origen/modelo. Tras un
`CheckpointCommitted`, el porcentaje y último uso atribuido esperan una nueva
respuesta de la misma ruta posterior a ese checkpoint. Los acumulados conservan
el consumo anterior.

## Documentos y herramientas

El panel distingue los últimos snapshots guardados de las lecturas actuales
para próximos turnos. `SystemInstructionsCommitted` aporta longitud UTF-8 y digest
de las instrucciones adicionales iniciales combinadas (proyecto, skills y goal).
`ToolCatalogCommitted` aporta un digest, no los esquemas ni su cantidad.
`McpRuntimeCommitted` es identidad de recuperación; su ausencia no demuestra
que no hubiera MCP. Cada fila conserva el número de evento y digest completo.

Las lecturas actuales usan los mismos loaders acotados del motor para AGENTS y
skills. Muestran documentos, bytes, procedencia y hashes sin estimar tokens por
archivo. Skills locales revalidan la selección guardada; un contenido cambiado
queda como activación no disponible y no acepta el hash automáticamente. Las
skills de marketplace informan los bytes del texto renderizado, sin inferir su
cantidad desde ese texto. La jerarquía inicial de AGENTS no es un inventario de
todas las instrucciones por directorio que las herramientas puedan incorporar
durante un turno.

Los servidores MCP habilitados son preferencias actuales, sin abrir conexiones.
Una inspección MCP sólo aparece si coincide con workspace y registro guardado
fresco. Su número de herramientas, bytes JSON y digest de metadatos se presentan
como observación independiente; no se certifican como catálogo congelado ni se
convierten a tokens.

El desglose en «Ver detalles» expone siete categorías tipadas sin interpretar textos de
filas: Sistema (bytes de la copia adicional validada), Herramientas (digest y
cantidad desconocida), Proyecto (documentos y bytes actuales), Skills (selección
local y bytes renderizados; bytes del marketplace separados), MCP (servidores
guardados y herramientas de una inspección coincidente), Subagentes (sin desglose
de definiciones ni tokens en este journal) y Conversación (entrada/salida acumulada
de respuestas de la ruta exacta). Sus valores conservan unidades distintas; no
son segmentos proporcionales de la ventana ni estimaciones de tokens por origen.
Una selección de skill obsoleta conserva su cantidad guardada, pero no informa
bytes activados; un error de journal/checkpoint no aporta snapshots ni uso a las
categorías históricas.

## Evidencia y límites

La vista inicial se construye en `composer_context_ui`: popover de hasta 320 px
lógicos, altura natural y controles compactos de 28 px. Muestra ocupación,
capacidad y espacio disponible; no mezcla esos datos con consumo histórico,
listas de instrucciones ni hashes. Encabezado, cierre y acciones permanecen
visibles; sólo el cuerpo se desplaza cuando falta espacio. Escape y el clic
exterior cierran el panel y devuelven el foco al medidor.

«Compactar» usa la acción real del host y aparece sólo en una conversación nativa
del proyecto actual. Permanece deshabilitado, con explicación, si hay permisos o
recuperación pendientes, ejecución activa, cambio de rama, otra compactación,
historial no verificable o ausencia de mensajes compactables. «Compactando»
refleja la operación de la sesión seleccionada y evita envíos duplicados. El
host revalida sesión, workspace, historial y disponibilidad antes de iniciar la
operación; el estado de presentación no concede autoridad. Una sesión recién
compactada espera nuevos mensajes antes de ofrecer otra compactación. No se
inventa una acción para la pantalla inicial ni se borra el historial guardado.

El medidor usa la fracción exacta `used_tokens / capacity`, independiente del
porcentaje entero legado. Un uso positivo menor al 1% tiene un arco proporcional
y etiqueta «<1%»; cero medido y ausencia de medición son distintos. El arco y la
cifra cambian a advertencia al 80% y a límite al 100%, sin animación continua.
El tooltip ofrece los conteos exactos. Tras una respuesta nueva se actualiza;
no estima tokens de una respuesta que todavía está transmitiéndose.

La barra de 4 px distingue uso medido y capacidad restante. Si el único límite
conocido es el máximo de entrada, usa exclusivamente entrada y lo titula «Uso de
entrada». Los máximos omitidos, retirados o nulos siguen desconocidos incluso
si había una caché previa. Cada estado explica la ausencia: ninguna respuesta,
proveedor sin uso, ruta distinta, compactación o capacidad sin verificar. Tras
compactar no se muestra un cero inventado: se espera una respuesta medida.

Si falla la consulta del proyecto o del journal, el resumen indica «Consulta no
disponible» y el detalle conserva la causa. Ese error pertenece únicamente a la
sesión, ruta solicitada, proveedor y modelo que lo produjeron: no se confunde
con una sesión nueva ni arrastra las mediciones de una consulta anterior.

«Detalles» abre una vista separada de hasta 440 px, con navegación y cierre fijos:
entrada/salida de la última respuesta, caché y razonamiento incluidos en esos
conteos, límites independientes, las categorías tipadas, consumo histórico,
eventos y hashes completos. El cuerpo tiene scroll propio y no sustituye bytes
o cantidades por estimaciones de tokens. Abrir estas vistas no emite peticiones
al modelo. El composer cerrado no construye el árbol del popover.

### Referencias del rediseño

- [Cursor: context usage](https://cursor.com/docs/agent/prompting#context-usage):
  acceso circular junto al prompt y desglose por categorías.
- [VS Code: widget](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/contrib/chat/browser/widgetHosts/viewPane/chatContextUsageWidget.ts)
  y [detalle](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/contrib/chat/browser/widgetHosts/viewPane/chatContextUsageDetails.ts):
  fracción reportada, límite del modelo y navegación por teclado.
- [VS Code: compactar contexto](https://code.visualstudio.com/docs/agents/run/sessions/manage-sessions#compact-conversation-context):
  resumen del historial como acción explícita accesible desde el control de
  contexto. La referencia local `chatContextUsageDetails.ts` y su CSS separan
  ocupación, reserva y acciones; OpenIDE conserva sus propios componentes y
  validaciones, sin asumir las opciones de VS Code que no implementa.
- Referencia local OpenChamber, `contextUsageReading.ts`: desconocido tras
  compactación hasta que llega una medición nueva. Se conserva esa distinción.


Tres pruebas de `native_context_ui` verifican stores reabiertos, último uso,
sumas y cobertura de subconteos, falta de uso sin fallback, ruta antigua o
cambiada, dueño/proyecto ajenos, snapshot inválido y overflow sin mutación.
`native_context_http_reopens_checkpoint_refills_and_rechecks_exact_route_without_effects`
ejecuta desktop Plan por HTTP local, compactación semántica y otro Plan tras
reabrir la app. Verifica 94% → desconocido tras checkpoint → 98% con respuesta
nueva; conteos exactos de ambos responses, máximo/reserva independientes y
ausencia de doble suma del request de compactación. También cambia documentos,
endpoint, proveedor, modelo y límite de salida/perfil, prueba scope CLI/proyecto
ajeno y corrompe el archivo real de checkpoint, sin alterar el journal durante
consultas. El poll de modelos cambia/restaura ventana y reserva observadas, y
recalcula el porcentaje de la última respuesta sin emitir inferencias. Los
límites del modelo proceden de una entrada declarada de fixture;
estas pruebas no aceptan un proveedor externo ni credenciales reales.

`native_context_hosted_web_search_refreshes_exact_request_profile_at_commit`
usa un journal validado de fixture y el resolutor real del backend Responses:
los handlers de búsqueda alojada y esfuerzo invalidan/restauran la atribución
inmediatamente al cambiar/restaurar el perfil. No emite requests externos.
El test HTTP puede exportar opcionalmente el árbol real del composer a SVG/PNG
con `OPENIDE_CONTEXT_VISUAL_DIR`: uso inicial, post-checkpoint y refill reabierto,
en 420/900 píxeles, resumen compacto y detalles inicio/final del scroll con
movimiento reducido. El export
inspecciona pintura/layout; no certifica identidad de píxeles del host egui.

### Prueba de escritorio del rediseño

`scripts/context-desktop-e2e.py` ejecuta la ventana nativa dentro del compositor
privado de `scripts/headless-desktop-fixture.py`, con un proveedor HTTP de fixture
en loopback y datos separados de la cuenta del usuario. El bootstrap y el observer
están limitados al feature `editor-e2e`; el uso entra por respuestas SSE del motor
real, sin inyectar porcentajes en la vista.

La aceptación verifica una medición desconocida, 6 000 / 20 000 tokens (30%) y
19 000 / 20 000 (95%), incluido el arco efectivamente pintado. Comprueba apertura
por puntero y Enter, Escape y restitución del foco, Detalles/Volver, clic exterior
sin enviar el borrador, y resize nativo a 520 × 420 con rueda sobre el scroll.
Se ejecutó correctamente con exactamente dos peticiones al proveedor local.
El script produce `report.json` y siete capturas PNG para inspección.

`context_visual_tests` complementa ese recorrido con cero/desconocido,
fracciones menores al 1%, compactación, límites sólo de entrada y fuentes de
nombres largos. Su export optativo incluye resumen, detalles, acción de compactación disponible
y compactación en curso a 360/900 px y alturas de 320/420 px con las fuentes
de la aplicación. Las pruebas derivadas verifican disponibilidad por dueño,
workspace, permisos y recuperación; las visuales comprueban activación por
teclado, estado deshabilitado, ausencia de acción en Home y errores sin uso
obsoleto. Estas verificaciones no son un benchmark de 120 Hz.
