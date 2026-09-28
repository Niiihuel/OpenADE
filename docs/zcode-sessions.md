# Dock de sesiones de ZCode: mapa funcional

Análisis del clon `references/ZCode` al 26 de septiembre de 2026. Describe el
comportamiento visible y sus rutas de datos para orientar una implementación
original en Rust. En ZCode, una «tarea» del dock corresponde a una sesión de
agente; el índice de tareas agrega metadatos de navegación al historial de la
sesión.

## Estructura y vistas

| Área | Evidencia | Flujo verificado |
| --- | --- | --- |
| Cabecera fija | `packages/ui/src/WorkspaceSidebar.tsx:1251-1268` | `aside` vertical, franja superior de 48 px para arrastrar la ventana, seguida de Nueva tarea. |
| Accesos fijos | `WorkspaceSidebar.tsx:1266-1349`; `NewTaskButtonGroup.tsx:8-39` | Nueva tarea mide 32 px y tiene atajo. Debajo aparecen Centro de comandos (búsqueda), Automatizaciones y Plugins. |
| Cuerpo desplazable | `WorkspaceSidebar.tsx:1351-1389,1643-1653` | Toolbar y listas comparten un scroll vertical (`overflow-y-auto`); pie de cuenta/ajustes queda fuera del scroll. El modo agrupado usa encabezado de grupo sticky. |
| Vistas de tarea | `WorkspaceSidebar.tsx:207-220,1018-1227,1378-1419` | Predomina **Proyecto**; el usuario puede elegir grupos manuales, cronología o archivo. El menú permite ordenar por creación o última actualización y expandir/contraer grupos. |
| Proyectos | `WorkspaceSidebar.tsx:1430-1579`; `WorkspaceSidebarItem.tsx:741-791,1117-1137` | Secciones Proyecto y Conversaciones son plegables y reordenables. Cada proyecto contiene una lista plegable de tareas, botón para crear allí y «Mostrar más». Los proyectos abiertos se pueden reordenar con drag. |
| Fijadas y archivo | `WorkspaceSidebar.tsx:1362-1388`; `WorkspacePinnedTasksSection.tsx:113-155`; `WorkspaceArchivedTasksFlatSection.tsx:24-39,97-105` | Fijadas es una sección global encima de la vista principal, incluso al mostrar Archivo. Archivo usa lista propia. |
| Filas | `TaskList.tsx:355-379,427-479`; `TaskListItem.tsx:517-550,566-580,602-680` | Fila seleccionable por clic/teclado, relleno de selección, título, estado (error, no leído, en curso), proyecto/tiempo y acciones al hover. Menú contextual se monta para la fila activa. |

La preferencia de organización admite `grouped`, `project` y `chronological`,
con `project` y orden `updated` como valores iniciales. Se guarda en
`localStorage` para recuperar la vista tras reiniciar
(`packages/ui/src/lib/sidebarTaskPreferences.ts:3-16,27-69`). Los grupos
manuales tienen nombre, color, membresía y orden persistidos en el índice
(`packages/services/src/session/tasksDatabase/schema-v1.ts:35-77`).

## Flujos de una sesión

| Acción | Evidencia | Comportamiento real |
| --- | --- | --- |
| Crear | `WorkspaceSidebar.tsx:1267-1284,1603-1616`; `App.tsx:703-772`; `v4/SessionPane.tsx:2750-2817` | Nueva tarea abre un **borrador** en el proyecto o grupo elegido. La primera entrega puede promover una sesión precalentada; si no existe, `createSession` recibe el primer mensaje. Por eso crear un borrador y persistir una sesión no son necesariamente el mismo instante. |
| Seleccionar | `workspace-grouped-tasks/task-row.tsx:263-267,358-362`; `app-shell/useWorkspaceTaskNavigation.ts:66-105,123-155,190-197` | La fila pasa ruta, identidad de workspace e ID de tarea a navegación; activa el proyecto correcto, limpia el indicador no leído con control de versión y fija la tarea activa. La selección se bloquea durante una reconstrucción de modelo. |
| Renombrar | `TaskActionMenuContent.tsx:94-103`; `TaskList.tsx:147-270,383-410`; `services/src/zcode-agent/zcodeTaskServiceAdapter.ts:2919-2974` | Un diálogo edita el título. Se marca `titleOverridden`, se actualiza el índice y se intenta sincronizar `renameSession` con el almacén v4. Una respuesta fallida de sincronización no descarta el título del índice. |
| Fijar / desfijar | `TaskActionMenuContent.tsx:81-93`; `WorkspacePinnedTasksSection.tsx:265-328`; `zcodeTaskServiceAdapter.ts:2992-2999` | El menú y el botón de la fila cambian la membresía de Fijadas. La UI mueve el ítem de forma optimista y revierte ante fallo. |
| Archivar | `TaskActionMenuContent.tsx:104-115`; `WorkspaceGroupedTasksSection.tsx:1084-1128`; `zcodeTaskServiceAdapter.ts:3029-3040` | Archivar saca la tarea de la lista activa y la lleva a Archivo; allí se puede desarchivar. La membresía se persiste en el índice. |
| Eliminar | `WorkspaceArchivedTasksFlatSection.tsx:273-318`; `DeleteAllArchivedTasksButton.tsx:56-95`; `zcodeTaskServiceAdapter.ts:2855-2916` | La eliminación individual está en Archivo y exige confirmación. También existe eliminación de todos los archivados con confirmación y resultado de lote. No hay acción de borrar directamente en el menú normal de tarea. |
| Marcar no leído | `TaskActionMenuContent.tsx:116-127`; `useWorkspaceTaskNavigation.ts:129-155`; `zcodeTaskServiceAdapter.ts:3001-3026` | El menú puede marcar una tarea no leída. Abrirla limpia el indicador de forma persistente con comparación de la marca anterior para no perder una notificación nueva. |
| Menús adicionales | `TaskActionMenuContent.tsx:127-229`; `workspace-grouped-tasks/task-context-menu-content.tsx:45-180` | Según vista y capacidad: abrir en split pane, mover de grupo o arriba, explorar carpeta, copiar ruta del proyecto/tarea/log/ID, ver trayectoria del modelo y enviar feedback. Algunas opciones se ocultan o desactivan en remoto, móvil o sólo lectura. |
| Buscar | `WorkspaceSidebar.tsx:1286-1300`; `command-center/CommandCenterDialog.tsx:451-523,640-657,854-898,986-1034`; `services/src/session/taskIndexRepo.ts:1801-1828` | El icono Buscar abre el **Centro de comandos**; su ámbito Conversaciones busca tareas por título y texto indexado, muestra fragmentos y navega a la tarea. `TaskFindDialog` es otra herramienta: busca dentro de la conversación o los cambios de archivo (`quickpick/TaskFindDialog.tsx:20-37,53-119`), no en el listado de sesiones. |
| Scroll y paginación | `WorkspaceSidebar.tsx:1351-1360,1537-1547`; `lib/workspaceTaskPagination.ts:3-38`; `TaskList.tsx:470-479`; `WorkspaceTimelineTasksSection.tsx:99-101,909-916`; `WorkspaceGroupedTasksSection.tsx:1480-1516,1725-1738` | Cada proyecto empieza con límite de cinco tareas y permite «Mostrar más». La cronología carga en pasos de 20. En la vista de grupos hay cabecera sticky y lista superior virtualizada. |

## Persistencia de ZCode

El índice local de navegación usa SQLite en `~/.zcode/v2/tasks-index.sqlite`
(`packages/services/src/paths.ts:185-188`;
`packages/services/src/session/taskIndexRepo.ts:486-535`). Su tabla `tasks`
guarda `workspace_key` + `task_id` como clave, título, agente/proveedor,
modo/modelo, tiempos, no leído, fijada, archivada, borrada y
`title_overridden` (`tasksDatabase/schema-v1.ts:3-33`). Los grupos tienen
tablas de membresía y orden (`schema-v1.ts:35-77`). El historial del agente
tiene además un almacén de sesiones CLI basado en SQLite, cuya API crea
sesiones y escribe mensajes/entradas
(`apps/zcode-cli/packages/adapters/src/storage/session-store/sqlite-session-store.ts:225-251,310-360`).
El índice y la sesión se sincronizan, por ejemplo, al renombrar
(`zcodeTaskServiceAdapter.ts:2934-2974`). Esta separación explica por qué
el dock puede consultar, ordenar y filtrar sin cargar toda la conversación.

## Estado de OpenIDE en Rust

| Flujo | Implementado | Diferencia concreta |
| --- | --- | --- |
| Shell de sesiones | `apps/agents-window/src/view.rs` (`sidebar`, `session_row`) | Nueva tarea, proyecto, lista con scroll, sección de fijadas, archivo y menú de fila. El sidebar puede mostrar sesiones o archivos. Aún faltan Centro de comandos, Automatizaciones y Plugins como accesos completos. |
| Crear y reabrir | `apps/agents-window/src/app.rs` (`new_session`, `open_session`, `start_run`) | Nueva tarea es un borrador; la sesión se crea al enviar el primer prompt. Reabrir carga eventos y transcript. Los borradores se persisten por workspace y sesión. OpenIDE bloquea el cambio de tarea mientras el agente corre. |
| Gestión | `app.rs` (`refresh_sessions`, acciones `session_*`); `crates/openide-core/src/store.rs` (`rename`, `set_pinned`, `set_archived`, `delete`) | Renombrar, fijar, archivar, restaurar y borrar una archivada con confirmación son reales y persistentes. Falta borrado por lote y grupos manuales. |
| No leído | `store.rs` (`mark_unread`, `clear_unread_if_matches`); `app.rs` (`open_session`) | Hay marca persistente con comparación para no borrar un aviso posterior; abrir la tarea la limpia. Aún no hay acción visual para marcar una tarea no leída. |
| Historial e integridad | `store.rs` (`list_with_diagnostics`, `load`) | Metadatos JSON por sesión, log JSONL y aislamiento de archivos corruptos al listar. La UI avisa cuántas sesiones no se pudieron leer. El título manual prevalece sobre el primer prompt. |
| Búsqueda y paginación | `store.rs` (`query_sessions`, `SessionQuery`) | La API de Rust filtra por uno o más workspaces y ámbito (activa sin fijar, fijada, archivada, todas), ordena por creación o actualización, busca texto literal en título y conversación visible y devuelve fragmentos, total, offset y `has_more`. Todavía no está conectada a una pantalla de búsqueda o «Mostrar más». |
| Organización | `app.rs` (`refresh_sessions`); `view.rs` (`sessions_sidebar`) | La UI agrupa por proyecto y ordena según actividad; no hay selector de vistas por cronología/grupos, expansión persistida ni reordenamiento drag. |

La consulta nueva es deliberadamente un contrato estable sobre el almacén
actual, no un índice persistente. Escanea eventos JSONL legibles y acota a
200 000 caracteres por sesión; omite del texto buscable los deltas de streaming,
entradas/salidas de herramientas y eventos internos. Para muchas sesiones,
un índice dedicado evitará releer logs en cada pulsación. Debe actualizarse
durante la escritura de eventos y reconstruirse de forma segura para sesiones
anteriores. La UI puede empezar con consulta al confirmar la búsqueda y
paginar 20 resultados por paso.

Mantener la ruta completa de `workspace` como clave de agrupación evita
confundir proyectos con el mismo nombre visible. El siguiente trabajo de
navegación es conectar un Centro de comandos a `query_sessions`, crear la
preferencia de vistas y el esquema de grupos manuales. El Browser, los split
panes y las automatizaciones dependen de contratos de UI/ejecución separados.
