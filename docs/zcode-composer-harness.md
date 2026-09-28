# Composer y harness de ZCode: mapa para OpenIDE

Análisis del clon local `references/ZCode` para orientar una implementación
**original en Rust**. Las rutas y líneas son puntos de lectura, no código a
trasladar. Este mapa distingue el **ZCode Agent** de la selección de modelos:
el tipo de agente visible de ZCode es `glm` (`packages/shared/src/providers.ts:1-14`,
`zcode-agent-policy.ts:5-24`), mientras que el catálogo de proveedores/modelos
del runtime vive en `packages/provider-node/src/provider-registry-runtime.ts:1-70`.
No hay en estas rutas un contrato equivalente a los perfiles de procesos CLI
de OpenIDE; `codex`, `claude`, `gemini` y `zcode` son integraciones de OpenIDE
con procesos externos, con menor observabilidad que el harness nativo.

## Recorrido de una solicitud en ZCode

| Etapa | Fuente | Comportamiento comprobado |
| --- | --- | --- |
| Borrador | `packages/ui/src/v4/composer/composerDraftStore.ts:1-43`; `ConversationComposer.tsx:1234-1272,1398-1418` | Guarda texto, estado del editor, modo y selección de modelo por sesión o draft en `localStorage`. Congela el borrador al enviar, lo restaura si la admisión falla y limpia sólo la revisión que realmente se envió. Los adjuntos no se guardan en ese borrador. |
| Configuración por envío | `packages/ui/src/v4/composer/composerSubmissionConfig.ts:7-45`; `V4ComposerModeControls.tsx:60-86,138-170`; `V4ComposerToolbar.tsx` | Valida proveedor/modelo/opciones contra el catálogo y congela `modelSelection`, modo y `planEnabled` para el envío. El modo de permisos y la marca Plan son controles distintos. |
| Admisión del compositor | `packages/ui/src/v4/ConversationComposer.tsx:1094-1136,1142-1183,1286-1418` | Puede enviar texto, adjuntos o referencias. Bloquea adjuntos sin preparar, entradas en modo `reject` y envíos sin configuración válida. Si el turno está ocupado, el botón puede pasar de Stop a envío en cola según el contenido. Conserva el draft si la orden se bloquea o necesita confirmación. |
| Envío al host | `packages/ui/src/v4/SessionPane.tsx:2597-2636,2862-2895,2921-2944` | Convierte la intención en `sendText` con configuración y referencias adjuntas. La admisión `accepted` delimita cuándo el compositor puede limpiar el texto; `choice` pide confirmación de la cola. Una barrera ordena cambios de configuración y envío. |
| Inbox de comandos | `apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/command-inbox.ts:107-209,223-245,310-446` | Serializa decisiones por sesión, deduplica `commandId`, comprueba epoch/revision, conserva inputs vivos en cola y devuelve ACK con resultado de admisión. Evita que una repetición de transporte vuelva a ejecutar la orden. |
| Inicio del turno | `apps/zcode-cli/packages/core/src/runtime/methods/turn.ts:76-149,215-337,362-427,591-612` | La orden entra en una cola cancelable; el runtime inicializa contexto y hooks, persiste sesión/input, emite inicio y ejecuta el loop. |
| Loop modelo ↔ herramientas | `apps/zcode-cli/packages/core/src/runtime/methods/turn-loop.ts:37-105,106-220`; `turn-model-step.ts:209-259,446-469,645-732`; `turn-tools.ts:43-180,180-242,286-426` | Antes de cada solicitud considera compactación, MCP, herramientas y recordatorios. El proveedor emite texto, razonamiento y llamadas de herramienta; las llamadas se programan, sus estados/resultados se persisten y regresan al contexto antes del siguiente paso. |
| Proyección en vivo y recuperación | `apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/v4-gateway.ts:746-769,809-952,963-1015`; `apps/zcode-cli/packages/adapters/src/storage/session-store/sqlite-session-store.ts:609-672` | El gateway ordena/aplica eventos al estado proyectado, soporta suscripción y rehidratación; SQLite guarda sesiones, mensajes, partes, entradas e inputs admitidos. |

### Permisos y herramientas

El registro de herramientas conserva contrato del modelo, esquema, metadatos
de lectura/destrucción/concurrencia y capacidad de permiso
(`apps/zcode-cli/packages/core/src/tool/registry.ts:11-24,102-138`). El
scheduler ordena dependencias y agrupa llamadas que puede ejecutar en paralelo
(`tool/scheduler.ts:9-67,69-95,97-168`).

La decisión de permiso usa herramienta, input, riesgo, modo, plan, reglas de
proyecto y capacidad de la herramienta; responde `allow`, `ask` o `deny`
(`permission/service.ts:28-78,90-151`). Una petición `ask` genera evento,
espera respuesta del broker con cancelación/timeout y vuelve a validar el
input si fue modificado por hooks (`tool/executor/permission-flow.ts:35-154,157-240`;
`permission/broker.ts:18-113`). La UI presenta la interacción y envía una
orden `resolveInteraction`, conservando el estado del botón mientras espera ACK
(`packages/ui/src/v4/V4InteractionDialogs.tsx:113-121,191-224,319-352`).
La ruta por defecto del broker sin cliente niega la solicitud
(`permission/broker.ts:27-36`). El modo `yolo` puede permitir sin diálogo,
pero `alwaysAsk` se trata antes de ese atajo (`permission/service.ts:129-151`).

El streaming de ZCode incluye deltas de texto y razonamiento, llamadas de
herramienta y recuperación de stream. Puede persistir una herramienta pendiente
antes de ejecutarla, y mantener emparejados `tool_call` y resultado al
recuperarse (`turn-model-step.ts:209-259,645-732`;
`runtime/methods/streaming-tool-coordinator.ts:73-130,171-209,230-320`). La
compactación combina estimación de tokens o uso reportado por el proveedor,
reserva de salida, microcompactación y resumen (`compact/policy.ts:8-64,68-157`;
`runtime/methods/turn-loop.ts:67-105`).

## Mejoras incrementales ya integradas en la ventana

La ventana Rust ya usa niveles estructurales de Zai Dark con trazas neutras en
gris, sin los acentos azules del tema de referencia (`theme.rs`). La navegación
agrupa sesiones persistidas por proyecto y titula cada una con la primera línea
de su primer prompt (`app.rs`, `refresh_sessions`; `view.rs`, sidebar). Los
separadores de los docks izquierdo y derecho son arrastrables y conservan un
ancho mínimo para la conversación (`app.rs`, `handle_dock_drag`; `view.rs`,
`dock_layout`). La lista de Actividad permite seleccionar herramientas; en una
ventana estrecha sólo se expande en línea el detalle de la seleccionada
(`app.rs`, `apply_action`; `view.rs`, timeline y panel Actividad).

Estos cambios mejoran la presentación y la interacción locales. El compositor
guarda borradores de texto por proyecto y sesión en `composer-drafts.json`, con
debounce de 350 ms y guardado al cambiar de sesión (`store.rs`,
`save_draft/load_draft`; `app.rs`, `save_draft/restore_draft`). Además recupera en memoria el prompt cuando el
turno falla, se cancela o se desconecta **antes de producir salida**, siempre
que el usuario no haya escrito otro prompt (`app.rs`, `restore_pending_prompt`). El borrador
enviado se borra al arrancar el worker, antes de un ACK de admisión; el prompt
restaurado por fallo vuelve a guardarse con el mismo debounce. El harness ya
puede esperar una aprobación por llamada de herramienta; la ventana muestra la
llamada y permite aprobarla o denegarla una sola vez. La solicitud entra en el
log antes de mostrar los botones y `SessionStore` registra la decisión en un
journal sincronizado antes de entregarla al runner; los errores de guardado
niegan la llamada (`app.rs`, `answer_permission/process_event`; `store.rs`,
`record_permission_decision`).

## Comparación con el Rust actual

| Área | OpenIDE hoy | Brecha y prioridad |
| --- | --- | --- |
| Borrador y envío | `SessionStore::save_draft/load_draft` y `app.rs` (`save_draft/restore_draft`) guardan texto por proyecto/sesión. `start_run` valida texto/ruta, inicia el worker y borra el borrador; `restore_pending_prompt` restaura el prompt si falla antes de emitir salida y programa su guardado. `view.rs` aporta editor multilínea y Enviar/Detener. | **P1:** limpiar el borrador tras una admisión verificable y conservar la revisión exacta enviada. El `start()` actual no ofrece ACK de admisión. |
| Agentes y modelos | `crates/openide-cli/src/lib.rs` ejecuta perfiles de CLI como procesos; `app.rs` (`start_run`) elige entre ese backend y el nativo. El nativo recibe endpoint/modelo escritos en Opciones. | **P1:** separar `CliProfile` de `ModelSelection`, introducir catálogo/capacidades por backend y validar antes de enviar. Las CLIs externas pueden seguir siendo opacas si no ofrecen protocolo estructurado. |
| Permisos | `crates/openide-harness/src/tools.rs` ofrece flags individuales y niega escritura/comandos por defecto. `runner.rs` y `permission.rs` añaden `Allow/Ask/Deny` por herramienta, con timeout y cancelación. `app.rs` (`process_event/answer_permission`) sólo muestra una solicitud tras anexarla al log y registra la decisión con `SessionStore::record_permission_decision` antes de responder al runner; ante fallo, deniega. El journal de decisiones es sincrónico e idempotente. El toggle aún activa escritura y comandos juntos. | **P1:** exponer permisos individuales y llevar la coordinación al runtime para recuperación sin ambigüedad. La solicitud y decisión se guardan en archivos distintos, sin una transacción común ni ACK de admisión. Siguen pendientes reglas por proyecto/sesión, concesiones duraderas y revalidación de input tras hooks como en ZCode. Separar Plan del permiso cuando exista Plan real. |
| Persistencia y admisión | `crates/openide-core/src/store.rs` guarda metadata y eventos JSONL; `app.rs` (`process_event/poll_run`) persiste desde el hilo UI y reconstruye el historial al desconectar. | **P1:** registrar la intención antes de ejecutar y hacer que el runtime sea dueño del log, con `commandId`, ACK e idempotencia. SQLite puede venir después; importa primero la coherencia entre admisión, estado y transcript. |
| Estado y cola | `AgentEventKind` en `crates/openide-core/src/event.rs` cubre inicio, deltas, permisos, herramientas, salida CLI y terminal. `app.rs` (`start_run`) impide otro envío mientras hay un run activo. | **P2:** declarar estados de turno y herramienta, errores recuperables, uso/retry; después añadir `startNow/queue/guide/choice` con cancelación y ACK. No mostrar una cola hasta que el runtime la admita. |
| Herramientas | `crates/openide-harness/src/runner.rs` ejecuta llamadas secuencialmente; `tools.rs` implementa cinco herramientas locales. | **P2:** registro extensible con esquema/capacidad, validación de argumentos y scheduler que sólo paralelice herramientas seguras. Después conectar MCP/plugins y resultados enriquecidos. |
| Streaming y contexto | `runner.rs` emite deltas de texto; `crates/openide-harness/src/context.rs` limita mensajes por caracteres y conserva los ID de herramientas. `crates/openide-cli/src/lib.rs` transmite stdout/stderr de procesos. | **P2:** uso/token budget del proveedor, razones de fin y resumen semántico. **P3:** ejecución de herramientas durante stream y recuperación ante corte; para CLI, limitar salida duradera sin perder el resultado final. |

## Orden sugerido para la próxima vertical

1. Definir en `openide-core` una intención de envío con ID estable, sesión,
   backend, modelo/modo cuando aplique, y un ACK explícito. Persistirla antes de
   iniciar el worker. El compositor conserva su borrador persistido hasta
   recibir `accepted` y sólo limpia la revisión enviada.
2. Exponer permisos individuales y coordinar solicitud, decisión y ejecución
   desde el runtime para recuperación después de reiniciar. El camino actual
   falla cerrado ante errores de guardado; faltan reglas por sesión/proyecto,
   concesiones persistentes y reconciliación entre log y journal.
3. Convertir la selección nativa en un catálogo de proveedores/modelos con
   capacidades; los perfiles CLI declaran qué acciones pueden mostrar o recibir.
4. Extender eventos de estado y recuperación, y luego agregar cola/guía y
   herramientas concurrentes. La proyección de `transcript.rs` deberá consumir
   los nuevos eventos sin inferir admisión ni aprobación desde texto libre.

Este orden mantiene como contratos separados el agente nativo, los proveedores
de modelo y los procesos CLI. ZCode aporta un ejemplo de coordinación entre
composer y runtime; OpenIDE debe implementar sus propios tipos, componentes y
políticas en Rust.
