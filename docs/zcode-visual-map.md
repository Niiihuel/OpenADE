# Mapa visual de ZCode para Agents Window

Referencia de diseño para implementar una interfaz original en Rust. Los archivos
citados están en el clon `references/ZCode`; OpenIDE adopta los niveles
estructurales de Zai Dark y construye sus propios componentes y marca. La vista
Rust está en `apps/agents-window/src/view.rs`. Las trazas de la implementación
actual usan grises, sin los acentos azules del tema de referencia.

## Fuentes principales

| Tema | Archivo y líneas | Hallazgo |
| --- | --- | --- |
| Criterios de producto | `references/ZCode/DESIGN.md:20-50` | Escritorio sobrio y denso, sesiones largas, salida de herramientas legible. Los temas visibles son Zai Light y Zai Dark; los temas CSS genéricos son bases de respaldo. |
| Roles de color | `references/ZCode/DESIGN.md:52-179` | Separación explícita entre fondo, superficies estructurales, superficie baja, tarjeta, entrada y flotante; texto primario/secundario/terciario y estados semánticos. |
| Tema oscuro activo | `references/ZCode/packages/ui/src/styles.css:606-711` | Valores concretos de Zai Dark. |
| Tipo | `references/ZCode/DESIGN.md:181-259`; `references/ZCode/packages/ui/src/styles.css:57-59,137-151` | Sans para UI, mono para rutas/comandos/código. Base 14 px y escala `text-ui-*`. |
| Espacio y radio | `references/ZCode/DESIGN.md:263-340` | Unidad 4 px; radio según contenedores anidados, con excepción para la caja principal del chat. |
| Iconos y controles | `references/ZCode/DESIGN.md:342-357` | Icono normal 16 px; alturas habituales 24/28/32/36 px. |

### Tokens de referencia de Zai Dark

| Rol | ZCode | Aplicación en OpenIDE |
| --- | --- | --- |
| Fondo | `#161616` (`--color-background`) | Lienzo de conversación. |
| Header / panel | `#202020` (`--color-header`, `--color-panel`) | Barra y paneles estructurales. |
| Sidebar | `#161616` (`--color-sidebar`); frame Windows/Linux `#2b2b2b` (`--color-background-win-alt`) | En escritorio, navegación un tono más clara que los paneles. |
| Superficie baja | blanco 5 %; hover blanco 10 % | Agrupaciones suaves y hover. |
| Tarjeta / entrada / flotante | `#2b2b2b` | Mensaje del usuario, editor y detalle expandido. |
| Borde / borde hover | blanco 10 % / 15 % | Separadores y contornos discretos. |
| Hover / selección | blanco 5 % / 10 % | Estados de filas; `TaskListItem` usa selección como relleno. |
| Texto | neutral-300; 60 % y 30 % del mismo para secundario y terciario | Tres niveles de lectura. |
| Acción primaria | blanco con texto negro | Sólo la acción principal, sin llenar paneles enteros. |
| Estado | éxito `#46bf72`, error `#ff5c5c`, aviso `#ff8a30` | La vista Rust usa rojo `#ff5c5c` para fallos; los otros estados siguen siendo referencia de ZCode. |
| Trazas | usuario `#60a5fa`, agente `#2dd4bf`, herramienta `#f59e0b`, resultado `#38bdf8` | `theme.rs` usa grises: usuario `#ebebeb`, agente `#d4d4d4`, herramienta `#b8b8b8`, resultado `#a2a2a2`; errores `#ff5c5c`. No se usan los acentos cromáticos de ZCode para las trazas. |

Los valores de ZCode provienen de `styles.css:609-711`. OpenIDE ya define
**nombres semánticos** en `theme.rs` (`BACKGROUND`,
`PANEL`, `CARD`, `INPUT`, `BORDER`, `FOREGROUND_SUBTLE`, `DESTRUCTIVE`) para evitar
que un solo `SURFACE` resuelva todos los niveles.

Escala tipográfica: 18, 16, 14, 13, 12 y 10 px (`text-ui-xl`, `lg`, `base`,
`caption`, `sm`, `xs`). La escala usa la variable base `--ui-font-size: 14px`;
9 px queda reservado a ejes del grafo de workflows. Espaciado: 4 px para
icono/texto, 8 px para controles, 12 px para filas densas, 16 px para
tarjetas/paneles y 20–24 px entre secciones (`DESIGN.md:263-280`). Radios:
primer contenedor `rounded-xl`, luego `lg` → `md` → `sm`; botón o entrada
normal `rounded-lg`; caja principal del compositor puede ser `rounded-2xl`
(`DESIGN.md:283-340`).

## Estructura de la vista v4

| Región | Fuente exacta | Estructura y adaptación |
| --- | --- | --- |
| Shell | `references/ZCode/packages/ui/src/app-shell/WorkspaceShellLayout.tsx:99-140,1530-1641,1664-1745` | Sidebar de 264 px por defecto y mínimo, ancho máximo 50 %; handle de 4 px; contenido principal mínimo 320 px. Marco de conversación con header y posibilidad de terminal o panel lateral. OpenIDE separa la navegación izquierda de un marco compartido para conversación y panel derecho; entre estos últimos hay un divisor de 1 px. Los separadores ajustables reservan 320 px de conversación en los tamaños de escritorio previstos. |
| Panel lateral derecho | `references/ZCode/packages/ui/src/app-shell/AnimatedSidePanePanel.tsx:912-1048`; `sidePaneLayout.ts:1-2` | Pestañas en el marco de trabajo con ancho inicial de 45 % y mínimo de 240 px. OpenIDE presenta Contexto, Archivos, Actividad, Cambios de Git y vistas previas; se puede ocultar, abrir y ajustar su ancho. Las pestañas se muestran por páginas según el ancho y un lanzador ocupa el panel cuando no queda ninguna abierta. |
| Sidebar por workspace/tareas | `references/ZCode/packages/ui/src/WorkspaceSidebar.tsx:1251-1419`; `references/ZCode/packages/ui/src/TaskList.tsx:383-453`; `references/ZCode/packages/ui/src/TaskListItem.tsx:542-555` | Espacio de título de 48 px; Nueva tarea, búsqueda, automatizaciones/plugins; área desplazable con toolbar, fijadas, agrupadas por workspace/otras vistas y archivo. Filas compactas `rounded-lg` con selección por relleno. OpenIDE muestra tareas persistidas agrupadas por proyecto, fijadas, archivo, renombrado y eliminación confirmada. `Ctrl+K` busca acciones, sesiones y archivos en un Command Center. Faltan vistas funcionales de automatizaciones/plugins. |
| Header | `references/ZCode/packages/ui/src/WorkspaceHeader.tsx:135-205`; `references/ZCode/packages/ui/src/app-shell/WorkspaceShellLayout.tsx:1687-1745` | Barra de workspace de 48 px con proyecto/tarea y acciones. `ConversationHeader.tsx:35-57` sólo añade acciones flotantes al pane, sin segundo encabezado de altura completa. OpenIDE usa un header central de 48 px con título de sesión, acceso a Opciones y apertura del panel derecho; la ruta se consulta en Contexto u Opciones. |
| Pane de agente | `references/ZCode/packages/ui/src/v4/SessionPane.tsx:4581-4625,4641-4708,4720-4758` | Composición vertical: header flotante, panel de estado (plan, meta, subagentes, Git), timeline y compositor pasado como `bottomDock`. OpenIDE ya usa header, timeline desplazable y compositor fijo. Muestra estado textual junto al compositor y en Contexto; aún no tiene datos ni panel de plan, subagentes o Git. |
| Timeline | `references/ZCode/packages/ui/src/v4/ConversationTimeline.tsx:1722-1758,1777-1904,1906-1949`; `references/ZCode/packages/ui/src/v4/conversationLayout.ts:3-22` | Scroll vertical con gutter estable, turnos históricos, cola viva, columna centrada que llega a `max-w-4xl` y compositor en la misma columna. Draft `max-w-2xl`. Dock `sticky bottom-0` dentro del scroll; botón volver al final si el usuario se aleja. OpenIDE ya centra una columna de lectura de ancho máximo, agrupa herramientas reales y mantiene el composer fijo fuera del scroll. |
| Filas de conversación | `references/ZCode/packages/ui/src/v4/ConversationTurnGroup.tsx:378-422,184-303`; `references/ZCode/packages/ui/src/v4/ConversationRowView.tsx:1901-1991,2095-2170` | Mensajes y trabajo del agente se agrupan por turno; llamadas de herramienta tienen renderer específico. En OpenIDE: fila de usuario diferenciada, texto del agente sin tarjeta elevada repetida, salida de proceso en bloque mono de altura limitada. |
| Herramientas | `references/ZCode/packages/ui/src/ToolCallBlocks/ToolLayout.tsx:21,303-358`; `references/ZCode/packages/ui/src/ToolCallBlocks/ToolSummaryRow.tsx:164-234`; `references/ZCode/packages/ui/src/ToolCallBlocks/renderers/execute.tsx:290-318` | Resumen compacto en una fila (icono, tipo, texto, estado, chevron), detalle desplegable con 8 px de separación. El comando completo va en mono dentro de panel bordeado. OpenIDE une `ToolStarted`/`ToolFinished` por ID. La fila de la conversación y cada entrada de la lista Actividad son seleccionables: el detalle aparece en Actividad cuando el panel derecho está visible y, en ventanas estrechas, sólo se expande en línea la herramienta seleccionada. Al reabrir un turno sin evento final aparece un aviso de interrupción, aunque la herramienta sin resultado todavía se etiqueta «En curso». |
| Compositor y configuración | `references/ZCode/packages/ui/src/v4/ConversationComposer.tsx:2018-2099,2137-2165,2187-2291`; `references/ZCode/packages/ui/src/v4/composer/V4ComposerToolbar.tsx:984-1088`; `references/ZCode/packages/ui/src/v4/composer/V4ComposerModeControls.tsx:40-81,99-186` | Editor principal con contexto arriba; modo/plan a la izquierda, modelo/thought/uso y Enviar o Detener a la derecha. El selector de modelo se alimenta de catálogo y el modo es elección separada; el error se presenta arriba de la caja de entrada. OpenIDE ya tiene editor multilínea, botón de motor y Enviar/Detener; el modelo y endpoint nativos se editan en Opciones. Si un turno falla antes de producir salida, el prompt se restaura en memoria si la entrada sigue vacía. Faltan adjuntos, catálogo de modelos, modos separados y controles de uso. |

En ZCode, el modo `plan` es una marca independiente de las opciones de
permisos (`V4ComposerModeControls.tsx:40-81,143-185`). No conviene que
OpenIDE muestre esos modos como universales: cada CLI y el harness nativo
tienen capacidades distintas.

## Navegación y estados del panel derecho

ZCode modela el panel como un registro de pestañas con ID estable, tipo, alcance
y pestaña activa (`packages/ui/src/lib/workspaceSidePane.ts:516-562`).
`AnimatedSidePanePanel.tsx:410-450,929-1060` filtra las pestañas visibles para
la sesión y corrige la activa al cerrar o cambiar de contexto. La barra tiene
un botón para abrir otra pestaña, acceso al resumen de pestañas y tratamiento
del desbordamiento horizontal. `SidePaneTabOverview.tsx:125-265` reúne búsqueda,
pestañas abiertas y pestañas cerradas recientemente que se pueden recuperar.

Cuando no hay pestañas visibles, `AnimatedSidePanePanel.tsx:814-849` presenta
un estado centrado «Open tab» con lanzadores para Review, Terminal y Browser.
Es un estado del **panel entero**, distinto de un árbol de archivos vacío o de
una revisión Git sin cambios. Los estados del árbol separan carga, error,
directorio vacío y filtro sin coincidencias
(`WorkspaceFileTreeList.tsx:98-116`, `WorkspaceFileTree.tsx:769-773`).
`GitPane.tsx:108-157,471-544` distingue Git ausente, carpeta sin repositorio,
ausencia de cambios y ausencia de cambios del último turno.

ZCode carga cada directorio cuando se expande y virtualiza el árbol con margen
de 12 filas (`useWorkspaceFileTreeData.ts:197-299`,
`WorkspaceFileTree.tsx:191-195`, `WorkspaceFileTreeList.tsx:150-172`). También
virtualiza la revisión Git; el comentario en `GitPane.tsx:300-324` atribuye
tareas superiores a 600 ms a montar cientos de archivos. En estas listas no
hay paginación numérica: la navegación por pestañas y la ventana visible de
filas resuelven dos problemas diferentes. OpenIDE conserva Archivos en el panel
derecho por la estructura solicitada por el usuario; en ZCode el árbol se abre
desde `WorkspaceSidebar.tsx:1667-1681` y el archivo abierto ocupa una pestaña
de vista previa (`AnimatedSidePanePanel.tsx:1186-1207`).

## Aplicación concreta a la vista Rust

La implementación actual usa `theme.rs` para los niveles neutros de Zai Dark y
la escala, componentes originales de `openide-ui` para bordes y tipografía
sans/mono, y `view.rs` para la navegación (176/200/264 px según ancho inicial)
y el marco de conversación con panel derecho opcional de al menos 240 px. El
header de conversación mide 48 px y el compositor queda fijo abajo. Los
separadores permiten ajustar ambos anchos durante la sesión. La navegación
agrupa las sesiones persistidas por proyecto, separa las fijadas y archivadas,
y muestra títulos derivados del primer prompt o editados por el usuario. Por
debajo de 800 px, el panel derecho desaparece y sólo la
herramienta seleccionada muestra su detalle dentro de la conversación.
`transcript.rs` agrupa eventos de herramienta por ID y consolida el streaming
de texto en una sola respuesta. El selector de motores contiene el harness
nativo y las CLIs detectadas; el motor elegido aparece en el compositor. La
ruta del proyecto se muestra en Contexto y se edita en Opciones.

Persisten diferencias estructurales: no hay terminal PTY, Browser embebido,
adjuntos ni controles de workflows. El panel Contexto resume estado e
historial; la revisión Git vive en su propia pestaña, con paginación de
archivos, y aún no hay plan ni subagentes proyectados como ZCode. El árbol de
Archivos usa filas virtuales para acotar el trabajo cuando hay muchos nodos.
Estos cambios siguen siendo implementación original en Rust.

La recuperación temprana del prompt es una mejora local del compositor. La
admisión con ACK sigue pendiente. El harness ya emite solicitudes de permiso y
espera una decisión, y la ventana muestra controles de Aprobar/Denegar para la
llamada concreta. La decisión se registra en un journal sincronizado antes de
permitir el efecto, y los fallos de escritura deniegan la llamada. Siguen
pendientes el ACK de envío y la recuperación coordinada entre log, journal y
runtime; su flujo se detalla en `docs/zcode-composer-harness.md`.
