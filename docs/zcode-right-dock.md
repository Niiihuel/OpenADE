# Dock derecho de ZCode: mapa para implementarlo en Rust

## Estado de OpenIDE (septiembre de 2026)

El registro Rust `apps/agents-window/src/dock.rs` ya implementa identidad,
scope por proyecto/tarea, selección, cierre, orden, colapso y ancho del dock.
OpenIDE presenta el árbol de archivos como pestaña **Archivos** del dock
derecho, junto a Contexto y Actividad. La sidebar izquierda muestra sólo
sesiones. Seleccionar un archivo abre su vista previa como otra pestaña derecha;
las rutas se mantienen relativas y se validan al leer. Esta ubicación responde
a la preferencia explícita del proyecto; el árbol de ZCode observado abajo
aparece como overlay de su sidebar izquierda.
La búsqueda local `search_project_files` recorre el workspace con límites,
devuelve rutas relativas y omite enlaces simbólicos y carpetas generadas; está
lista para que el Command Center la llame en un trabajo de fondo.
También funcionan Contexto, Actividad y la vista de pestañas abiertas. El
Browser embebido, la terminal PTY, los diffs y medios aún requieren sus
respectivos backends. Los apartados siguientes conservan el análisis de ZCode
y las prioridades originales para comparar lo implementado con lo pendiente.

Análisis del checkout local `references/ZCode` en `29628c9acdb8`. Las rutas y números de línea remiten a ese snapshot. Este documento describe el comportamiento observado y propone un orden de implementación para OpenIDE; no prescribe copiar código de ZCode.

## 1. Qué es el dock derecho

El panel derecho de ZCode es un **registro de pestañas de un workspace**, no un panel fijo de “Contexto”. Cada pestaña tiene un tipo y un dueño. Al cambiar de tarea, el registro se conserva, pero sólo se muestran las pestañas visibles en el nuevo contexto. El contenido Browser permanece montado aun con el panel cerrado para no perder la página ni la conexión del agente. El terminal horizontal inferior es una superficie distinta del terminal que se puede abrir como pestaña derecha. Fuentes: `packages/ui/src/app-shell/WorkspaceShellLayout.tsx:1634-1668,1902-1935`; `packages/ui/src/lib/workspaceSidePane.ts:516-561,1105-1133`; `packages/ui/src/app-shell/AnimatedSidePanePanel.tsx:410-435`.

```text
WorkspaceShellLayout
├─ Sidebar izquierda
│  └─ WorkspaceFileTree (overlay de la sidebar)
└─ ResizablePanelGroup horizontal
   ├─ conversation-column (mínimo 35 %)
   │  ├─ conversation: header + transcript + composer
   │  └─ AnimatedTerminalPanel: terminal inferior independiente
   ├─ handle vertical
   └─ AnimatedSidePanePanel: dock derecho
      ├─ barra de pestañas: overview, tabs, +, cerrar dock
      └─ contenido según tipo: Browser, PreviewPane, Git, Terminal, etc.
```

`DirectoryBrowser.tsx` es un selector de directorios con `onSelect`/`onCancel`, usado para elegir una ruta; no es el explorador del dock derecho (`packages/ui/src/DirectoryBrowser.tsx:44-85`).

## 2. Mapa de componentes y responsabilidades

| Superficie | Fuente | Responsabilidad observable | Datos que recibe / emite |
| --- | --- | --- | --- |
| `WorkspaceShellLayout` | `packages/ui/src/app-shell/WorkspaceShellLayout.tsx:1416-1485,1634-1668,1864-1884,1902-1935` | Coloca conversación, terminal inferior y dock derecho. Inyecta handlers de apertura desde mensajes, herramientas y enlaces. Mantiene el dock fuera del cambio de vista Automations/Plugin Store para conservar Browser Guest. | Workspace, tarea activa, visibilidad; callbacks `onOpenBrowserUrl`, `onOpenCodeViewer`, `onOpenPlanDetail`, subagentes, bash, workflows. |
| `useAppPanels` | `packages/ui/src/hooks/useAppPanels.ts:131-347,349-380,381-529,1290-1565` | Dueño de estado y efectos del dock: abrir, activar, cerrar, ordenar, recordar, enrutar eventos de Browser y administrar preferencias por tarea. | `WorkspaceSidePaneState`, colapso, navegación, metadatos, pestañas cerradas. |
| `workspaceSidePane` | `packages/ui/src/lib/workspaceSidePane.ts:6-120,516-620,943-1036,1050-1176,1485-1535,1900-2159` | Tipos de pestaña y funciones puras de apertura, identidad, filtrado por scope, cierre y orden. | `tabs[]`, `activeTabId`; retornos inmutables. |
| `taskSidePaneMemory` | `packages/ui/src/lib/taskSidePaneMemory.ts:7-31,53-70,72-143` | Caché en memoria por **workspace**, máximo 50 entradas LRU. Guarda tabs, colapso por owner, fuente Git y URLs. | `workspaceIdentity` o path; `taskId` deliberadamente excluido de la clave. No equivale a persistencia en disco. |
| `AnimatedSidePanePanel` | `packages/ui/src/app-shell/AnimatedSidePanePanel.tsx:403-500,681-802,912-1058,1060-1305,1317-1377` | Marco, resize, tab strip, launcher, render por tipo, conservación de instancias. | Estado resuelto, workspace, owner y callbacks. |
| `SidePaneTabTrigger` / `SidePaneTabOverview` | `packages/ui/src/app-shell/SidePaneTabTrigger.tsx:95-196`; `SidePaneTabOverview.tsx:134-265` | Icono/título/cierre, clic central, menú contextual, drag; overview de pestañas abiertas y recientemente cerradas con búsqueda. | Activar, cerrar actual/otras/todas, reabrir. |
| `UnifiedBrowserView` / `HumanBrowserView` / `BrowserUseSidePaneContent` | `packages/ui/src/browser-use/UnifiedBrowserView.tsx:208-225,408-439,1001-1076`; `HumanBrowserView.tsx`; `BrowserUseSidePaneContent.tsx:22-96` | Navegador embebido y estado de guest. Browser humano y Browser controlado por agente comparten la vista, pero tienen identidad y preferencias distintas. | URL, navegación, título/favicon, ready/error, residency, screenshot, viewport. |
| `WorkspaceFileTree` / `PreviewPane` | `packages/ui/src/workspace-file-tree/WorkspaceFileTree.tsx:88-186,554-556,581-585`; `WorkspaceSidebar.tsx:1664-1694`; `PreviewPane.tsx:480-665,873-935,1475-1534` | Árbol en sidebar izquierda; su selección abre una pestaña `code-viewer` derecha con scope del archivo. Preview carga/renderiza texto, diff, imagen, media, PDF y PPTX. | Ruta y scope; apertura de preview, editor externo y enlaces. |
| `SidePaneTerminalPane` / `AnimatedTerminalPanel` | `packages/ui/src/SidePaneTerminalPane.tsx:5-49`; `app-shell/AnimatedTerminalPanel.tsx:36-150` | PTY en pestaña derecha frente a PTY horizontal inferior. Ambos conservan sesión tras ocultarse, pero el primero usa identidad de tab y registro propio. | `sessionId=tab.id`, workspace, cwd, visible; abrir URL detectada. |
| Pestañas de contexto | `packages/ui/src/app-shell/AnimatedSidePanePanel.tsx:1101-1185`; `PlanDetailSidePane.tsx:15-75`; `SelectionSideChatPane.tsx:9-53`; `BackgroundBashOutputSidePane.tsx:11-133` | Plan, conversación secundaria, subagente, salida bash, workflow y artefactos. No son una única vista “Contexto”. | IDs de sesión/herramienta/run, contenido y acciones de salto a archivo/URL. |

## 3. Geometría y reglas visuales

| Elemento | Valor en ZCode | Fuente |
| --- | --- | --- |
| Ancho inicial del dock al expandirse | 45 % del grupo horizontal. `conversation-column` parte de 52 % si el dock está visible. | `app-shell/sidePaneLayout.ts:1-2`; `app-shell/WorkspaceShellLayout.tsx:1653-1656` |
| Dock derecho | Colapsado 0 px; mínimo 240 px; máximo 65 %. El handle ocupa 4 px. Sólo el botón colapsa: arrastrar hasta el mínimo no cierra el dock. | `app-shell/animatedSidePanePanelModel.ts:52-59`; `app-shell/AnimatedSidePanePanel.tsx:1339-1367` |
| Barra superior de tabs | 48 px. Una tab mide 28 px de alto, crece hasta 156 px y se contrae a 60 px; separación 4 px. Si ni el mínimo cabe, la fila se desplaza horizontalmente y el botón `+` se mueve fuera de la zona desplazable. | `app-shell/AnimatedSidePanePanel.tsx:947-990,1027-1046`; `app-shell/SidePaneTabTrigger.tsx:139-149`; `app-shell/sidePaneLayout.ts:4-27` |
| Menú `+` | Anchura 192 px. Ofrece conversación lateral si hay tarea, Review/Git si aún no está, terminal, Browser si está soportado y Developer Tools si está habilitado. Whiteboard figura comentado. | `app-shell/AnimatedSidePanePanel.tsx:681-802` |
| Resumen de tabs | Popover 288 px, búsqueda de 32 px, lista hasta 320 px. Busca título/hint/tipo; permite cerrar o reabrir. | `app-shell/SidePaneTabOverview.tsx:134-265`; `app-shell/sidePaneTabSearch.ts:1-69` |
| Barra de Browser | 48 px, padding horizontal 12 px, hueco 8 px; address input de 28 px. Back, forward, reload, dirección, responsive mode, element picker y menú de abrir fuera/DevTools. | `packages/ui/src/EmbeddedBrowserPaneParts.tsx:44-150` |
| Cabecera del preview | 40 px con breadcrumb desplazable y acciones; cuerpo ocupa el resto. Preview pesado se dibuja sólo si la pestaña está activa, dock visible, ancho visible ≥96 px y resize asentado; audio/video en fullscreen se conserva durante resize. | `packages/ui/src/PreviewPane.tsx:1524-1568`; `app-shell/animatedSidePanePanelModel.ts:1,62-92`; `app-shell/AnimatedSidePanePanel.tsx:1186-1207` |
| Terminal derecho | Padding 12 px dentro de la pestaña. | `packages/ui/src/SidePaneTerminalPane.tsx:36-49` |
| Terminal inferior | Cerrado por defecto; al abrir mide mínimo 140 px y máximo 50 % de la columna; colapsado 0 px; handle 4 px. | `packages/ui/src/hooks/useAppPanels.ts:189-200`; `app-shell/AnimatedTerminalPanel.tsx:89-121` |
| Transiciones | Opacidad del dock y terminal 200 ms; el contenido sigue montado tras la primera apertura. | `app-shell/AnimatedSidePanePanel.tsx:492-500,1368-1375`; `app-shell/AnimatedTerminalPanel.tsx:59-65,117-149` |

OpenIDE calcula el dock derecho con un mínimo de 240 px y reserva 320 px para el centro. El registro dinámico ya conserva pestañas de Contexto, Actividad y preview de texto por proyecto y tarea; otras vistas dependen de backends aún pendientes. La app inicia el dock visible, mientras ZCode lo inicia cerrado sin tabs (`packages/ui/src/hooks/useAppPanels.ts:189-200`).

## 4. Modelo de estado que conviene trasladar

**Registry y scope.** `WorkspaceSidePaneState` tiene `tabs[]` y `activeTabId`. Cada tab carga `id`, `type`, `workspaceKey`, `ownerTaskId` y datos de su tipo. Al crear tabs, ZCode congela workspace/owner/remoteSession; un evento tardío de Browser Use conserva su scope original (`lib/workspaceSidePane.ts:1076-1103`). La clave de memoria es `workspaceIdentity.trim()` o la ruta; todas las tareas del workspace comparten el registry, pero las pestañas normales se filtran por owner. Git y Developer Tools son globales al workspace. Browser Use se filtra por `sessionId`; subagentes y bash por `rootSessionId`; plan, chat lateral y workflow por `parentSessionId` (`lib/workspaceSidePane.ts:1054-1133`). La preferencia explícita de colapso es por owner, y al cambiar de tarea se restituye la tab visible preferida o la última visible (`lib/workspaceSidePane.ts:1135-1176`; `hooks/useAppPanels.ts:320-347`).

**Identidad de tabs.** Abrir un archivo vuelve a enfocar la pestaña de su `sourceKey` y actualiza la fuente. La clave incluye workspace, tipo y ruta; PPTX y media normalizan entradas `file`/específica al mismo recurso, y distintos contenidos de diff pueden coexistir porque la clave incluye hash de contenido (`lib/workspaceSidePane.ts:943-1005,1485-1535`). La búsqueda de reutilización compara también el owner, pero el **ID generado no lo incluye** y `activateSidePaneTab` reemplaza por ID: abrir el mismo archivo desde dos owners puede desplazar la primera pestaña. En Rust conviene resolver esta colisión incluyendo el owner en la identidad o haciendo la pestaña explícitamente global (`lib/workspaceSidePane.ts:996-1036,1498-1508`). Browser humano nuevo recibe UUID; URL de share puede reusar tab por URL (`hooks/useAppPanels.ts:381-438`). Terminal derecho recibe ID único y `cwd`; Git tiene tab singleton `git` (`lib/workspaceSidePane.ts:6-44,88-97,1214-1289,1589-1610`).

**Visibilidad y vida útil son independientes.** Tabs ocultas por tarea siguen en el registry. `TabsContent forceMount` preserva estado local; Browser y bash pueden obligar a montar el dock aun colapsado (`lib/workspaceSidePane.ts:537-556`; `app-shell/AnimatedSidePanePanel.tsx:929-945,1060-1105`). Terminal inferior se crea sólo en su primera apertura; luego se oculta sin cerrar PTY (`app-shell/AnimatedTerminalPanel.tsx:59-65,126-149`). Terminal derecho conserva PTY y scrollback por `tab.id` aun si el componente se desmonta al cambiar de workspace; se libera al cerrar esa tab o el workspace (`SidePaneTerminalPane.tsx:15-24`; `terminal/sidePaneTerminalSessionRegistry.ts`).

**Cierre real.** Cerrar tab activa elige vecina; cerrar otras/todas sólo afecta las tabs visibles al owner. El menú de recientes conserva hasta ocho tabs, excluye `browser-use` y `selection-side-chat`; al reabrir Browser crea una nueva identidad (`lib/workspaceSidePane.ts:1982-2048`; `hooks/useAppPanels.ts:1332-1347,1395-1534`). Browser pide al proceso principal cerrar el logical tab **antes** de quitar la UI; si falla, la UI queda intacta para evitar tabs fantasma que reaparezcan (`hooks/useAppPanels.ts:1350-1393`). Cerrar terminal libera el registro PTY (`hooks/useAppPanels.ts:1403-1407,1447-1450`).

**Normalización.** Al leer memoria, repara `activeTabId`, migra ordinales de chats laterales y elimina tabs de treemapping deshabilitado (`lib/workspaceSidePane.ts:564-620`). La memoria de tabs está en un `Map` del renderer, no demuestra por sí sola recuperación tras reiniciar la app; el Browser posee además snapshots/lifecycle del proceso principal (`lib/taskSidePaneMemory.ts:27-31,72-117`; `hooks/useAppPanels.ts:1350-1359`).

### Contrato Rust sugerido

Separar **estado puro del dock**, **efectos** y **vistas**. Las variantes sólo se agregan cuando existe una fuente de datos real.

```text
DockStore[WorkspaceId] -> DockState { ordered_tabs, active_by_owner, collapsed_by_owner, width_px }
DockTab { id, workspace_id, owner_scope, kind, opened_at }
DockKind = Context | Activity | FilePreview(source) | Diff(source)
         | Browser(browser_id) | Terminal(terminal_id) | Plan(tool_id)
         | Subagent(session_id) | BashOutput(work_id) | Review | ...
DockCommand = Open | Activate | Reorder | Close | CloseOthers | CloseAll
            | RestoreRecent | Collapse | Resize | BrowserNavigate | BrowserEvent
DockEffect = ReadFile | StartPty | StopPty | CreateBrowser | CloseBrowser | ...
```

Mantener `Context` y `Activity` actuales como dos tipos de tab durante la migración. Un `DockScope` explícito debe llevar `workspace_id`, `task_id` y, si se soporta remoto, `remote_session_id`. El reducer debe rechazar eventos viejos o de otro scope antes de aplicar cambios. Para recursos externos (PTY/webview), el estado de tab conserva la identidad estable; un administrador de recursos decide cuándo crear, suspender, restaurar o destruir el recurso. Este contrato es una propuesta de diseño, no una API presente en ZCode.

## 5. Flujos de datos y eventos

| Evento de usuario o runtime | Ruta observada en ZCode | Efecto que debe modelar Rust |
| --- | --- | --- |
| Clic en archivo del árbol izquierdo | `workspace-file-tree/WorkspaceFileTree.tsx:554-556` → `WorkspaceSidebar.tsx:1683-1693` → `useAppPanels.ts:349-364` → `openCodeViewerSidePane` | Crear/activar `FilePreview` con ruta **y** scope de origen; expandir dock. Un archivo de otro workspace se lee con su host original sin cambiar la conversación activa. |
| Clic en archivo/diff/enlace de mensaje o herramienta | `WorkspaceShellLayout.tsx:1864-1884,1257-1370` → handlers `onOpenCodeViewer`/`onOpenFileLink` | Despachar el mismo comando de preview; archivos y URLs no necesitan entradas duplicadas. |
| `+` → nueva tab | `AnimatedSidePanePanel.tsx:681-802` → handlers de `useAppPanels.ts:486-507,728-810,893-938` | Abrir por capacidad. Review singleton; cada terminal y Browser nuevo tiene ID propio; chat lateral sólo con tarea. |
| Activar / reordenar / cerrar | `SidePaneTabTrigger.tsx:95-196` → `useAppPanels.ts:1290-1329,1395-1504` → `workspaceSidePane.ts:2018-2159` | Mantener orden y active; cerrar recurso externo con autoridad; no cerrar tabs de otra tarea por accidente. Clic central cierra sin activar primero; drag empieza tras 4 px. |
| Overview | `SidePaneTabOverview.tsx:134-265` → `useAppPanels.ts:1506-1565` | Buscar tabs abiertas/recientes, activar, cerrar o restaurar; abrir dock al restaurar. |
| URL emitida por conversación o popup | `useAppPanels.ts:381-529` | Capturar scope en origen; si llega tarde, crear tab en workspace/tarea de origen sin robar foco. En plataforma sin navegador embebido, ZCode abre navegador externo. |
| Browser de agente | `useAppPanels.ts:531-702`; `workspaceSidePane.ts:99-120,1360-1482`; `BrowserUseSidePaneContent.tsx:22-96` | Aplicar ready/operation/visibility/close/suspend/restore usando `(workspace, session, browserId, generation, tabId)`; descartar generaciones viejas. Si se implementan capturas, el guest debe tener superficie renderizable aun con dock oculto. |
| Salida bash/subagente/plan/workflow | `WorkspaceShellLayout.tsx:1873-1883` → `useAppPanels.ts:829-938` → render en `AnimatedSidePanePanel.tsx:1101-1185` | Abrir tabs contextuales con ID de origen y owner. El plan usa markdown congelado como respaldo y se actualiza desde la proyección si el tool call sigue disponible (`PlanDetailSidePane.tsx:26-58`). |

## 6. Browser: dos modos y un backend propio

La pestaña **humana** usa `HumanBrowserView`; la del agente usa `browser-use` con `tabId` opaco asignado por main, `sessionId`, `browserId` y generación (`lib/workspaceSidePane.ts:6-24,99-120`; `AnimatedSidePanePanel.tsx:1060-1092,1264-1301`). El estado visual contiene URL, título, favicon, back/forward, loading, ready y errores (`embeddedBrowserHelpers.ts:71-105`; `UnifiedBrowserView.tsx:408-439`). El toolbar entrega la URL al navegador sólo después de validar/solicitar navegación; `UnifiedBrowserView` encola peticiones antes de `dom-ready` y deduplica `initialUrl` y requests externos por ID (`UnifiedBrowserView.tsx:208-225,760-871`). Las pestañas nuevas sin URL retrasan la creación del guest (`UnifiedBrowserView.tsx:1001-1005`). Hay estados de vacío, carga, fallo de página/guest y retry (`EmbeddedBrowserPaneParts.tsx:205-316`).

El guest no se puede equiparar a una textura de la UI Rust. ZCode depende de Electron `<webview>`, de eventos IPC del proceso principal y de CDP para el control del agente. Además, un guest suspendido no se monta; al activar se pide `ensureResident` y se ignoran eventos de generación vieja (`lib/workspaceSidePane.ts:553-556,1431-1482`; `hooks/useAppPanels.ts:1290-1319`). Al restaurar, main repone historial: el renderer omite `initialUrl` para no adelantarse con una navegación (`AnimatedSidePanePanel.tsx:1079-1083,1275-1280`). La captura de un browser-use oculto requiere temporalmente una superficie visible para el compositor, con interacción y accesibilidad desactivadas (`BrowserUseSidePaneContent.tsx:22-95`).

Para OpenIDE conviene definir primero una interfaz `BrowserBackend` con navegación, historial, eventos, ciclo de vida y captura. La UI del dock debe mostrar controles/estados con un backend simulado mientras se elige e integra un motor embebido real. El control del agente, las capturas y la suspensión quedan detrás de esa interfaz. Es una **secuencia de implementación propuesta**, no una equivalencia de capacidad actual.

## 7. Files y Preview: ruta completa

El **árbol** se superpone a la sidebar izquierda, no se muda al dock derecho (`WorkspaceSidebar.tsx:1664-1694`). Carga directorios de forma perezosa, mantiene expansión, búsqueda indexada, filtro de modificados Git, watcher/refresco y filas virtualizadas (`workspace-file-tree/WorkspaceFileTree.tsx:88-186,581-585,634-785`; `workspace-file-tree/useWorkspaceFileTreeData.ts:55-92,204,363-405,531-650`). Al abrir un archivo, el `CodeViewerSource` incluye ruta, workspace, identidad y sesión remota; eso evita leer desde el host equivocado (`WorkspaceSidebar.tsx:1683-1693`).

El Command Center solicita el índice de archivos del host en bloques y cede el
hilo entre bloques para conservar la respuesta al teclado; filtra sólo archivos
y exige que cada término de la consulta aparezca en el nombre o ruta relativa
(`command-center/CommandCenterDialog.tsx:103-119,562-598`;
`workspace-file-search/fetchWorkspaceFileEntries.ts:1-27`). La API Rust
`search_project_files(workspace, query)` tiene el mismo contrato visible de
resultados, con una implementación de recorrido acotado: hasta 12 niveles, 512
directorios, 16 384 entradas totales, 1 024 por directorio y 80 resultados.
Devuelve `truncated` y contadores si algún límite o error parcial impide una
búsqueda completa. Ignora `.git`, `node_modules` y `target`, no sigue enlaces
simbólicos, y verifica que cada directorio resuelto permanezca dentro del
workspace. La ruta encontrada sigue siendo relativa y el preview vuelve a
validarla al abrir. Esta función es **sincrónica**: la integración debe usar un
worker o caché, y descartar resultados de una consulta/workspace anterior.
Todavía no existe el índice, watcher, búsqueda remota ni filtro Git de ZCode.

El **preview** derecho acepta `file`, `code-review`, `text`, `patch`, `multi-file-diff`, `image`, `media`, `pdf`, `pptx` (`lib/codeViewer.ts:25-114`). Para texto lee hasta **256 KiB**; si el servicio informa truncamiento no ensambla trozos mal decodificados, sino que muestra tamaño excesivo (`PreviewPane.tsx:873-935`). Markdown y SVG alternan vista/código; hay wrap, breadcrumbs y apertura en editor externo (`PreviewPane.tsx:619-645,690-706,1479-1506,1524-1590`). PDF/PPTX/media usan servicios y estados especializados, no el lector de texto (`PreviewPane.tsx:937-1077,1322-1427`). El cuerpo pesado de diffs se posterga al ocultar o estrechar el dock y restaura scroll al reaparecer (`PreviewPane.tsx:1429-1473`).

## 8. Contexto, terminal y vistas avanzadas

La palabra “contexto” cubre varias tabs de ZCode: `plan-detail` muestra markdown de una herramienta con actualización desde la proyección y último contenido como respaldo; `selection-side-chat` monta una conversación hija con focus y scope; `subagent-session`/`subagent-directory` remiten a sesiones hijas; `bash-output` muestra salida en curso y seguimiento del scroll; `workflow-*` muestran runs, actores y artefactos (`lib/workspaceSidePane.ts:122-195,205-343,351-455`; `AnimatedSidePanePanel.tsx:1101-1185`; `PlanDetailSidePane.tsx:26-75`; `SelectionSideChatPane.tsx:9-53`; `BackgroundBashOutputSidePane.tsx:11-133`). Los tabs de workflow siguen accesibles para auditoría aunque el run salga de una proyección reciente (`lib/workspaceSidePane.ts:205-225`). El “Contexto” fijo de OpenIDE puede seguir siendo un resumen de workspace, pero no debería ser el único contenedor para estas vistas.

Hay dos elecciones de terminal: el inferior es un panel verticalmente redimensionable y se monta al primer uso; el derecho es una tab por PTY y no dibuja una segunda barra de tabs dentro (`AnimatedTerminalPanel.tsx:36-150`; `SidePaneTerminalPane.tsx:15-49`). La salida bash de una tarea es una vista **de lectura** de un trabajo en segundo plano, no un terminal interactivo (`BackgroundBashOutputSidePane.tsx:20-37,83-130`).

## 9. Prioridades para OpenIDE

| Orden | Entregable comprobable | Dependencia / razón |
| --- | --- | --- |
| P0: fundamento del dock | Registry `DockTab` + reducer de abrir/activar/reordenar/cerrar; owner/workspace explícitos; panel arrastrable 240 px–65 %, barra de tabs de 48 px; preservar `Context`/`Activity`. Tests de cambio de tarea, cierre de activa y colapso por owner. | Toda vista futura depende de identidad, scope y ciclo de vida. Sustituye `SideTab` binario (`apps/agents-window/src/view.rs:12-17,843-903`). |
| P1: Files + preview de texto/diff | Árbol de workspace en izquierda; click abre/reusa pestaña de archivo derecha; loading/error/archivo demasiado grande; diff y scroll. Tests de duplicado por ruta y de scope de workspace. | Da una utilidad real al dock y reutiliza el filesystem actual. El preview puede crecer por formatos sin rehacer tabs. |
| P2: terminal en tab | PTY por ID de tab y cwd, scrollback preservado al ocultar, liberación al cerrar. Mantener separada una eventual terminal inferior. Tests de PTY único por tab y cierre explícito. | Permite múltiples shells sin confundir salida de herramientas con terminal interactiva. |
| P3: Browser humano | Barra y estados; `BrowserBackend` embebido, navegación/historial/errores, título/favicon y ciclo de vida. Tests del enrutamiento de URL y de eventos tardíos por workspace. | El mock de toolbar se puede hacer temprano; la navegación real exige integrar un motor web. |
| P4: Browser de agente y contexto avanzado | Identidad main/backend, generación, suspensión, capturas; plan/subagente/bash/workflow como tabs según haya datos reales del harness. Tests de eventos obsoletos, no robo de foco, cierre con autoridad y captura oculta. | Depende de un backend Browser y de proyecciones del harness. |
| P5: refinamiento | Overview/búsqueda/reabrir, clic central, drag, animación, media/PDF/PPTX, revisión Git y editor externo. | Añade cobertura y paridad sin bloquear los flujos base. |

**Criterios transversales:** ningún evento de otra tarea debe activar la tab actual; una tab oculta no debe destruir su recurso; cerrar debe liberar exactamente el recurso correspondiente; un archivo remoto debe leerse en su scope; y un browser suspendido no debe reconstruirse con una URL que sobrescriba su historial. Estos criterios salen de las defensas explícitas de ZCode citadas arriba y conviene automatizarlos antes de añadir muchos tipos de tab.
