# Navegación de ZCode frente a OpenIDE

Mapa del clon local `references/ZCode` (26-09-2026). Las rutas son evidencia de
comportamiento; la implementación Rust es original. Este documento complementa
`zcode-sessions.md`, `zcode-composer-harness.md` y `zcode-right-dock.md`.

## Modelo general

ZCode navega en **tres niveles distintos**:

1. Tabs de workspace y Settings en la ventana. `tabStore.ts:257-339,365-442,492-503`
   abre/reusa workspaces, selecciona el siguiente al cerrar y abre Settings como
   tab. Al regresar de Settings conserva montado el workspace: la capa de
   ajustes lo cubre e inhabilita la interacción de fondo
   (`root/RootWorkspaceContent.tsx:105-199`).
2. Historial tipo navegador dentro de un workspace. Incluye tareas,
   Automations y Plugin Store, con identidad de workspace, deduplicación de
   vecinos, corte de rama futura y límite de 50 entradas
   (`lib/taskNavigationHistory.ts:1-179`). Las flechas de atrás/adelante están
   en `DesktopTopOverlay.tsx:160-189`; `useWorkspaceTaskNavigation.ts:66-197,
   200-398` resuelve el destino y cambia de workspace antes de seleccionar una
   tarea. Cuando se elimina una tarea, se retira del historial
   (`taskNavigationHistory.ts:181-224`).
3. Tabs de recursos en el dock derecho. Tienen identidad y owner propios; una
   tab de otra tarea no debe activarse ni cerrarse por cambiar de conversación
   (`lib/workspaceSidePane.ts:1054-1176,1982-2159`). Su overview permite
   buscar, activar, cerrar y reabrir recientes
   (`app-shell/SidePaneTabOverview.tsx:24-261`). Es navegación distinta del
   historial de tareas.

OpenIDE mantiene una sola ruta de workspace en `ViewState`, alterna sesiones y
archivos en la barra izquierda, y utiliza `DockState` para la derecha
(`apps/agents-window/src/view.rs:68-102,483-705,1292-1468`;
`apps/agents-window/src/dock.rs`). `navigation.rs` aporta un historial puro
`Task/Draft/Files/Settings` de hasta 64 entradas, aún pendiente de enlazar con
acciones de la ventana. `Files` guarda la tarea central seleccionada porque el
árbol cambia sólo la barra izquierda.

## Recorridos observados

| Recorrido | ZCode: ruta y conducta | OpenIDE: estado y siguiente paso |
| --- | --- | --- |
| Abrir proyecto / cambiar workspace | El menú del proyecto en el estado vacío busca workspaces abiertos, permite cambiar y abrir carpeta (`ChatEmptyState.tsx:198-420`). El store reusa tab por identidad, no sólo por ruta (`store/tabStore.ts:80-90,257-339`). | `change_workspace` actualiza la ruta en la pantalla de opciones (`app.rs:961-987`). Falta selector directo de proyectos recientes/abiertos y tabs superiores. Separar identidad de ruta antes de remoto. |
| Nueva tarea | Botón lateral con atajo abre draft; en barra superior aparece botón al plegar sidebar (`WorkspaceSidebar.tsx:1266-1284`; `DesktopTopOverlay.tsx:190-207`). El composer recibe nueva versión de foco incluso si ya estaba en draft (`v4/ConversationComposer.tsx:806-817,1056-1060`). | `new_session` guarda el borrador actual, limpia transcript y restaura draft (`app.rs:619-644`). Falta foco fiable del composer desde toda entrada y atajo global. |
| Elegir tarea | La fila comunica `taskId`, ruta e identidad. Activa workspace, limpia no leído y bloquea selección mientras se reconstruye el runtime (`useWorkspaceTaskNavigation.ts:66-197`). | `open_session` carga eventos/historial y cambia el proyecto (`app.rs:646-706`); bloquea si corre un agente. Enlazar la visita al historial sólo después de carga válida. |
| Atrás / adelante | Un único historial llega a tarea/Automations/Plugin Store. Reproducir no vuelve a insertar la entrada; si el destino local no tiene tab abierto, puede abrirlo; el remoto sin attachment falla cerrado (`useWorkspaceTaskNavigation.ts:230-398`; `app-shell/taskNavigationWorkspace.ts:1-40`). | El módulo puro `navigation.rs` ya cubre deduplicación, branching y límites. Falta UI en header, restaurador de destinos y poda al borrar sesión. |
| Centro de comandos | Sidebar → buscador global, con ámbitos Acciones/Tareas/Archivos y chips, snippets, recientes e historial (`WorkspaceSidebar.tsx:1286-1300`; `command-center/CommandCenterDialog.tsx:77-182,446-523,986-1067`). `TaskFindDialog.tsx:20-119` busca **dentro** de conversación/cambios y es otra acción. | No hay paleta global ni índice de texto. Primera vertical útil: comandos reales (nueva tarea, sesiones, archivos, ajustes), luego indexar sesiones/archivos. No mezclarla con buscar en transcript. |
| Sesiones/archivos a izquierda | Files superpone la lista de sesiones y conserva contexto central; el árbol carga directorios al expandir, con búsqueda/filtros/watcher (`WorkspaceSidebar.tsx:1664-1694`; `workspace-file-tree/WorkspaceFileTree.tsx:88-186,581-785`). | `SidebarMode::{Sessions,Files}` y `FileExplorer` perezoso funcionan; falta filtrar/buscar, watcher, revelar archivo seleccionado y teclado de árbol. |
| Composer → harness | Draft por sesión, envío congelado con configuración y ACK; estados ocupado/cola/Stop y permiso viven en la conversación (`v4/ConversationComposer.tsx:1094-1418`; `v4/SessionPane.tsx:2597-2636,2862-2944`). | Texto, backend, enviar/detener, draft persistido y permiso básico existen. Falta ACK duradero, cola, adjuntos, referencias, modelos/capacidades y configuración por envío. Ver `zcode-composer-harness.md`. |
| Abrir archivo | Click desde árbol o transcript llama al mismo handler de preview; guarda workspace de origen (`WorkspaceSidebar.tsx:1683-1693`; `app-shell/WorkspaceShellLayout.tsx:1257-1370,1864-1884`). | Click del árbol abre `FilePreview` y lector de texto. Falta click en referencias del transcript, diff y preview especializado. |
| Dock derecho | `+` abre vistas según capacidad; overflow/overview, click central, close others/all, drag, scope y recursos independientes (`AnimatedSidePanePanel.tsx:681-802`; `SidePaneTabTrigger.tsx:95-196`; `SidePaneTabOverview.tsx:134-265`). | Contexto/Actividad/Archivo, overview básico, cierre y ordenamiento por botón. Falta menú `+` con capacidades reales, buscador/recientes en overview, drag y tipos de recurso. Ver `zcode-right-dock.md`. |
| Terminal | Panel inferior y tab derecha son rutas independientes. El inferior conserva PTY al ocultarse y el derecho la conserva por ID de tab (`AnimatedTerminalPanel.tsx:36-150`; `SidePaneTerminalPane.tsx:15-49`). | No hay PTY ni navegación a terminal. Implementar backend y ciclo de vida antes de mostrar entrada activa. |
| Browser | Tab humana con URL, historial y estado; tab de agente tiene scope/generación distintos (`UnifiedBrowserView.tsx:208-225,760-905`; `hooks/useAppPanels.ts:381-702`). | No hay backend web embebido. La navegación UI debe diferenciar browser humano del browser del agente y descartar eventos tardíos por owner. |
| Ajustes | Settings es una capa sobre el workspace montado; la salida vuelve al contexto y gestiona la vista principal previa (`root/RootWorkspaceContent.tsx:105-199`; `app-shell/useWorkspaceMainViewSettingsExit.ts:14-50`). | `show_settings` sustituye centro con formulario (`view.rs:1029-1120,1470-1513`); conserva `ViewState` en memoria pero no hay secciones ni retorno por historial. |

## Atajos y foco

ZCode registra comandos de nueva tarea, abrir workspace, paneles, navegador,
terminal, revisión y ajustes en un catálogo para la paleta
(`quickpick/quickPickCommands.ts:47-220`). Los labels de teclado aparecen en
la app y el header (`App.tsx:127-133`; `DesktopTopOverlay.tsx:136-207`). El
matcher evita que un atajo imprimible con Shift robe entrada a un editor y
considera composición IME (`shortcuts/bindings.ts:119-198`). Escape cancela
edición o detiene generación según contexto; el botón de envío y Enter van por
la misma admisión (`v4/ConversationComposer.tsx:1493-1620,2065-2094`).

OpenIDE entrega eventos de teclado al árbol UI, pero `app.rs:1401-1470` no
registra atajos globales. Para evitar conflictos, el router de teclado debe
tener prioridad explícita: diálogo/menú > editor enfocado > comandos globales;
Ctrl+Enter del composer ya pertenece al editor. Atajos iniciales sugeridos:
Ctrl+N nueva tarea, Ctrl+K paleta, Alt+Left/Right historial, Ctrl+B sidebar,
Escape cerrar overlay o detener sólo donde haya cancelación explícita. Son
propuesta para OpenIDE, no equivalencia garantizada con bindings de ZCode.

## Secuencia de implementación priorizada

1. **P0 — navegación coherente:** conectar `navigation.rs` a seleccionar tarea,
   draft, Files y Settings; header atrás/adelante con disabled real; restaurar
   sin reinsertar; no registrar destino si falla la carga; limpiar rutas de
   tarea eliminada. Cubrir ida/vuelta entre dos proyectos y borrado de destino.
2. **P0 — foco y teclado:** foco al crear nueva tarea desde cualquier modo,
   shortcut dispatcher contextual, Escape predecible y foco restaurado al
   cerrar menús. Prueba manual con IME y editor multilínea.
3. **P1 — proyectos y búsqueda:** selector de workspace y recientes, Centro de
   comandos con acciones funcionales y búsqueda de sesiones; un buscador
   separado para transcript cuando haya índice de contenido.
4. **P1 — flujos del dock:** abrir preview desde árbol y transcript, overview
   buscable/reabrir, tabs ocultas por owner, cierre de recursos con autoridad.
5. **P2 — capacidades nuevas:** PTY y Browser con backends y lifecycle propios,
   previews de diff/media, Automations/Plugins sólo si existen módulos reales.

### Capturas útiles de la app de referencia

Para reproducir detalles de interacción que el código no deja claros, pedir
capturas del **mismo tamaño de ventana y tema**: estado vacío; draft con menú de
proyecto y composer abierto; tarea corriendo y permiso; sidebar con sesiones,
fijadas y archivo; Files expandido; dock con varias tabs y overview; Settings;
Browser/terminal. Por cada pantalla, anotar qué clic o atajo la abrió, cuál
era la tarea activa y qué sucedió al volver. Esto permite validar estado,
espaciado y navegación sin inferir comportamiento a partir de una sola imagen.
