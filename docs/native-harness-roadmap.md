# Harness nativo de OpenIDE: decisión y criterios de cierre

Decisión: 28 de septiembre de 2026. Referencia de Codex:
[`33a0f766`](https://github.com/openai/codex/tree/33a0f766a647208b471cfbcad889c67fd324ee04),
clonada en `references/codex` dentro del workspace y excluida por Git.

## Decisión

OpenIDE desarrollará y operará su propio harness en Rust en
`openide-harness`, con contratos durables en `openide-core` y presentación en
el framework `openide-ui`. Codex es una referencia técnica; el producto no
importa `codex-rs/core` como biblioteca ni lo necesita como proceso para las
sesiones nativas. Las nuevas tareas del composer usan sólo el harness propio;
el historial de las CLI conserva su origen y se consulta sin continuarlo desde
ese composer. La selección de modelos usa un
[catálogo propio](providers-models-architecture.md) obtenido de las APIs
oficiales de los proveedores admitidos, con caché local. El catálogo no
autoriza por sí solo la ejecución.

OpenIDE conserva la propiedad del ciclo del agente en Rust. La comparación con
[Codex](codex-rust-harness-reference.md) informa contratos de turno,
recuperación y herramientas, sin delegar la autoridad del journal o los
permisos de las sesiones nativas.

## Fronteras técnicas

| Capa | Dueño | Regla |
| --- | --- | --- |
| UI y estado visual | `openide-ui`, `agents-window` | Proyecta eventos del dominio y envía comandos con ACK; no ejecuta herramientas ni decide el resultado de un turno. |
| Sesiones y persistencia | `openide-core` | IDs estables de sesión/turno/comando, log ordenado e idempotente, checkpoints y replay. |
| Harness | `openide-harness` | Loop, modos, presupuesto de contexto, compactación, catálogo de herramientas, permisos, ejecución y cancelación. |
| Proveedores | Adaptadores Rust por protocolo y registro local versionado | Listas directas desde APIs oficiales, con mapeos JSON declarativos, última copia local válida y hash de credencial/definición. Los esfuerzos aparecen sólo para el ID exacto y un protocolo que sepa enviarlos. |
| Referencias externas | `references/` | Lectura y comparación. Si se incorpora código Apache de Codex, conservar `LICENSE`, `NOTICE` y atribuciones aplicables. |

## Estado y trabajo pendiente

Las casillas representan una capacidad completa **verificada**, no solo la
existencia de un tipo o un control visual.

| Capacidad | Estado actual | Criterio de cierre pendiente |
| --- | --- | --- |
| Bucle modelo-herramienta, streaming, cancelación y permisos básicos | Parcial | Journal síncrono y prueba HTTP local de Plan→Build→Resume tras efecto incierto→compactación/checkpoint con reapertura de stores. La aceptación `process_cuts_reopen_plan_build_resume_and_checkpoint_without_repeating_effects` añade catorce cortes básicos reales de proceso en admisión, petición/respuesta, permisos, inicio/efecto/resultado de herramienta y cierre, con reapertura, efecto único y checkpoint/plan preservados (Linux, Rust 1.95.0, 2026-09-29). Se añadieron ocho fronteras de hooks Pre/Post en Unix; dos matan el runner con SIGKILL después de sincronizar el recibo del hook y seis salen después de HookStarted/diagnóstico/HookFinished. Replay exige verificación manual sin repetir hook/herramienta; los recibos se preservan hasta completar Resume y checkpoint. La prueba `checkpoint_publication_process_cuts_keep_last_committed_history_after_reopen` verifica tres cortes adicionales tras resumen, candidato publicado y marcador sincronizado; faltan aceptación con proveedor real, interrupciones dentro de syscalls y cierre de las demás fronteras externas. El ciclo HTTP/SSE ahora incluye el corte real `route_committed` antes de la primera petición: reapertura conserva la ruta exacta, sin duplicar inferencia ni efectos. Marcadores propios distinguen Pensando/Generando, permisos, herramientas, error e incertidumbre desde eventos; animan sólo actividad real de la sesión, no una llamada histórica inconclusa. Streaming normal y Resume entregan deltas guardados antes del terminal; cuatro pruebas de tail, cinco App TCP/HTTP/SSE, dos de cola y cuatro del worker comprueban primera pintura, UTF-8 dividido, cancelación, cortes, reapertura y entrega sin duplicados. El desktop consume hasta 64 registros por frame; no es un límite de memoria del canal ni una garantía de latencia. Ver `docs/native-streaming.md`. La aceptación `active_checkpoint_acceptance::active_build_checkpoint_process_cuts_reopen_and_resume_http_without_repeating_tool` cierra el orden Plan→Build→permiso→efecto confirmado→compactación HTTP del prefijo Plan con Build activo→checkpoint→reopen→Resume. Dos cortes reales sin destructores dejan CheckpointCommitted como tail o interrumpen el primer AssistantDelta del paso 2. Comprueba nuevo lease/stores, efecto único, ruta/catálogo/contexto congelados y Plan/Goal íntegros; el texto parcial no entra en history. Linux y presupuesto explícito de fixture, sin proveedor real ni corte dentro de syscall.  `native_anthropic_cycle` añade el recorrido Messages propio: Plan/Build HTTP/SSE, dos cortes de proceso en respuesta comprometida/inicio de lectura, verificación manual, continuación con firmas/bloques opacos/resultados exactos, resumen semántico/checkpoint/reopen y Resume Idle sin efectos. Cuatro rutas/perfiles distintos se rechazan antes de append/request/lectura. Transporte loopback; no cuenta real. Ver docs/native-anthropic.md. |
| Admisión durable del turno | Parcial | `TurnAdmitted` idempotente, lease exclusivo entre procesos y secuencia verificada. Un lock transaccional por sesión serializa lectura, admisión, append y borrado entre handles/procesos; el tail cache se invalida al cambiar el journal o recrear un ID. La UI distingue las admisiones no iniciadas y ofrece recuperación. Catorce cortes básicos y ocho fronteras de hooks Pre/Post (Unix) con reapertura y recuperación del lease pasan en `native_http_cycle`; faltan aceptación con proveedor real y cortes durante escritura/publicación de journal/checkpoint y otras extensiones. |
| Modos Plan y Build | Parcial | El modo está persistido y Plan ofrece sólo lecturas locales certificadas. `PlanProposed` y `PlanUpdated` guardan snapshots acotados que sobreviven a compactación y Resume; `update_plan` sólo se anuncia en Build. La UI muestra los pasos y ofrece «Construir este plan» una vez antes de la admisión Build. Faltan aceptación con modelo real y clasificación extensible de efectos. |
| Responses API y proveedores | Parcial | OpenAI Responses persiste IDs, uso y elementos opacos en `ModelResponseCommitted`; Chat Completions conserva ID y uso cuando el stream los entrega, exige `[DONE]` y un `finish_reason` de `tool_calls` o `function_call` antes de ejecutar herramientas, y rechaza `finish_reason=length`. Pruebas HTTP locales. El registro incorpora OpenAI, OpenRouter, DeepSeek, Groq, Mistral y xAI, y admite proveedores añadidos para protocolos implementados. Catálogo y backend usan capacidades de IDs exactos, sin reglas por prefijo; una matriz revisada cubre sólo los pares con evidencia adicional. La aceptación opt-in `live_openrouter_catalog_exposes_exact_limits_and_reopens_cache` consultó directamente la API pública y reabrió el cache el 2026-09-29: 635 IDs exactos, 393 pares de límites con herramientas; sin inferencia ni credenciales. Faltan pruebas de generación con cuentas reales y ampliar adaptadores verificados. Los adaptadores Chat y Responses limitan cada respuesta a 64 llamadas y 8 MiB agregados retenidos; Responses aplica también un límite al output opaco serializado. Los deltas vacíos no impiden retry antes de texto observable. 27 pruebas backend y 21 Responses verifican transporte y rechazo previo a devolver herramientas, incluidos finales grandes y muchas llamadas vacías. No es un límite de RSS, ni el máximo de herramientas ejecutadas por turno.  Messages de Anthropic se implementa como tercer protocolo propio y admite IDs arbitrarios de proveedor. Catálogo/discovery comparten autenticación declarativa y paginación completa, conservan la lista anterior ante página inválida y extraen esfuerzos/capacidades del ID exacto. Límites mapeados nulos/cero o retirados siguen desconocidos; niveles vacíos y familias de capacidades omitidas/nulas/inválidas conservan autoridad explícita en discovery/cache, sin heredar límites, niveles o autorizaciones anteriores. HTTP local con refresh y cache reabierto comprueba la retirada; selector, presupuesto y tooltip comparten esa resolución exacta. code_execution no se interpreta como herramientas cliente. El backend exige cierre SSE completo, conserva thinking firmado/redacted y vincula los opacos al modelo/endpoint; rechaza protocolos/rutas mezclados y redirects sin reenviar la clave. HTTP/SSE local y cortes reales; generación con cuenta Anthropic, aliases resueltos y otros bloques aún pendientes. Ver docs/native-anthropic.md. |
| Tokens y compactación semántica | Parcial | Tres cortes reales de proceso en publicación mantienen el checkpoint previo o activan únicamente el candidato con marcador durable, con reopen y retry (`checkpoint_publication_process_cuts_keep_last_committed_history_after_reopen`, Linux 2026-09-29). Resumen semántico, checkpoint con checksum, comando manual y compactación automática en ejecución y resume con pruebas locales. Las solicitudes Responses con imágenes y límite verificado consultan el conteo exacto de `POST /responses/input_tokens` antes de responder y después de compactar; un conteo desconocido o fallido no admite la solicitud bajo esa puerta. Para modelos sin límites verificados, se permite el turno sin compactación automática y la compactación manual registra esos límites como desconocidos, sin inventar ventana ni reserva. Ejecución, automatizaciones, Resume, hijos, compactación manual y límites de contexto resuelven ahora el par exacto declarado por la cuenta/API, con respaldo revisado sólo cuando no hay una observación autoritativa de límites para ese ID; una respuesta nueva con máximos mapeados desconocidos no recupera un par antiguo. La prueba HTTP `refresh_accepts_removals_and_retains_last_valid_models_after_bad_response` demuestra cambio de ventana, reapertura del cache, conservación ante refresh inválido y aceptación/rechazo del mismo prompt antes del envío. Las pruebas de resolución rechazan herencia por nombre/proveedor, pares parciales y negación de herramientas incluso para IDs revisados. `native-output-limits.json` vincula un tope al proveedor/ID/protocolo exactos; Responses lo envía, el resolvedor baja únicamente la reserva y la huella cambia. `configured_output_cap_reopens_changes_budget_and_backend_identity`, `output_limits_reopen_and_match_exact_route`, `output_limit_is_validated_and_part_of_recovery_identity` y pruebas HTTP verifican reapertura, envío y exclusión del conteo, con rechazo de valores inválidos. `plan_build_uncertain_effect_reopens_resumes_and_replays_checkpoint` rechaza un tope cambiado o retirado después de reabrir el store sin emitir eventos ni repetir efectos; con el tope original termina Resume y reabre el checkpoint. Settings → Proveedores permite guardar/retirar el tope de la ruta activa; dos pruebas de acciones/reapertura y layout a 360/620/1300 px verifican valores inválidos, estado running y preservación de otra ruta. Faltan aceptación de inferencia con proveedor real, aceptación del editor con modelos reales y ruta efectiva de routers, confirmar conteo para cada modelo anunciado y ampliar ventanas verificadas. La proyección `NativeContextUiState` reemplaza el porcentaje anterior: último uso reportado de una ruta idéntica, entrada/salida/total y subconteos cache/razonamiento sin doble suma; acumulados con cobertura explícita y límites por ID exacto. `native_context_http_reopens_checkpoint_refills_and_rechecks_exact_route_without_effects` usa Plan y compactación HTTP reales, reopen y otro Plan: checkpoint limpia el medidor hasta el siguiente uso y conserva acumulados. Rutas cambiadas, legacy sin procedencia, uso ausente, overflow y stores corruptos no producen porcentaje inventado. Bytes de AGENTS/skills, digests y cantidades de herramientas son datos separados, sin atribución de tokens ni total de facturación; la inferencia de compactación no aporta uso al journal actual. Ver `docs/native-context-projection.md`. La tarjeta inicial Contexto se redujo a un máximo de 320 × 200 px: porcentaje, cociente, barra de entrada/salida y dos filas. Las siete categorías con bytes, cantidades y procedencia están únicamente en Ver detalles. Sin medición ni modelo explícito no inventa porcentaje o ruta de fallback; exportación HTTP y pruebas de geometría/reapertura separan este cambio visual del conteo efectivo.  Messages cuenta la solicitud completa mediante /messages/count_tokens, también texto/herramientas. TokenBudgetPolicy y checkpoints conservan input_limit_tokens independiente de una ventana total; la reserva de salida no se carga al límite de entrada. CompactService también valida el presupuesto del pedido efectivo de resumen antes de inferir/publicar un candidato; cuatro pruebas HTTP verifican rechazo, ventana/entrada, fallo/timeout/cancelación y checkpoint reabierto. Dos pruebas de presupuesto y una de store reabierto verifican conteo, umbrales y rechazo de límites inválidos; native_anthropic_cycle compara input123 con límites200/100 y output4096 sin inferir tras el rechazo. La UI conserva el gráfico compacto y compara sólo entrada cuando esa es la capacidad publicada. Default4096 es política de solicitud acotada por máximo exacto conocido, no capacidad del modelo. |
| Resume/recovery | Parcial | `StoreError` conserva como I/O los fallos del lector JSON; `listing_does_not_hide_general_filesystem_failures` demuestra que el listado no los oculta como corrupción (corregido y verificado en Linux 2026-09-29). Replay estricto, estados de recuperación, motor durable por pasos y worker conectado a la UI. El marcador v2 vincula proveedor, endpoint, modelo, modo, workspace, catálogo, instrucciones y perfil exacto de solicitud antes de la primera petición. Las decisiones de permiso registradas antes de una caída se aplican una vez al reanudar el padre o hijo. La barra lateral y la sesión seleccionada muestran recuperación para turnos raíz e hijos; los lotes abortados se descartan explícitamente y los efectos inciertos se verifican manualmente. Un journal inválido muestra diagnóstico sin reintento automático. Se reconstruyen catálogos históricos sin `send_input`, sin PTY y anteriores a `update_plan`. MCP HTTP moderno remoto HTTPS con bearer u OAuth en Windows fija identidad autenticada; HTTP legacy y loopback siguen cerrados para resume. La aceptación de catorce cortes básicos de proceso comprueba los estados raíz AdmittedNotStarted, ReadyForModel, ModelInFlight, ToolReady, PermissionPending, ToolOutcomeUncertain e Idle tras reabrir el store. Faltan aceptación de punta a punta con proveedor real y pruebas con servidor MCP y vault reales, hijos e interrupciones dentro de syscalls de publicación de checkpoints. `ModelRouteCommitted` versión 1 guarda proveedor/ID exactos y digests de endpoint y perfil, sin URL ni secretos. Runner sincroniza el evento antes de pedir al modelo; Resume valida identidad antes de requests, hooks o tools. Tres pruebas Resume y el ciclo HTTP con corte real verifican mismatch sin append/efectos, reutilización tras reopen y legacy sin atribución retroactiva. Un fallo al guardar la ruta impide pedir al modelo. El backend ya fue construido por el host; esto no afirma una validación anterior al lookup de credenciales. Ver `docs/native-model-route.md`. Resume observa eventos sólo después de append y drena el tail aceptado al fallar/cancelar, conservando ModelInFlight y recuperación manual. Cinco pruebas `recovery_streaming_*` verifican fallos reales de append/observador y corte tras ModelResponseCommitted. El transcript recupera el texto autoritativo y las citas desde ese commit, aun sin AssistantMessage, y completa sin duplicarlo; el prefijo parcial no entra en el historial del modelo. HTTP local y stores reales; no certifica fsync individual por delta ni proveedor remoto. |
| Registro de herramientas y ejecución | Parcial | Catálogo congelado, digest persistido por turno y validación JSON Schema Draft 2020-12 antes de pedir permiso o ejecutar. El scheduler agrupa hasta cuatro lecturas locales certificadas, sincroniza cada inicio antes del efecto y confirma resultados en orden; el replay exige verificar manualmente las lecturas sin resultado tras una caída. `apply_patch` nativo acepta Add/Delete/Update y `Move to:` de texto acotado bajo el mismo permiso que `write_file`, preevalúa el lote sobre un estado virtual, valida contextos y destinos antes de cambiar archivos y revierte archivos y directorios creados si un commit reporta fallo. La reversión de varios archivos no es una transacción atómica ante corte de energía ni frente a reemplazos concurrentes hostiles. Mutaciones y MCP siguen seriales. Faltan trazas completas y clasificación extensible de efectos. |
| Aislamiento de comandos | Parcial | Windows tiene perfil MXC de escritura sólo en workspace y red denegada, con pruebas de proceso real para archivo hermano, junction, loopback, entorno, cancelación y restauración de ACL. Los comandos sin sandbox reciben un entorno mínimo, sin secretos arbitrarios heredados. `start_command`/`poll_command`/`write_stdin`/`terminate_command` mantienen procesos acotados por sesión nativa en modo host, y los revocan al retirar acceso. Las reglas de prefijo argv Ask/Deny para `run_command` y `start_command` tienen editor en Settings, carga fail-closed y huella durable; una recuperación valida la misma huella. Los fallos ambiguos después de iniciar un proceso quedan como efecto incierto. Faltan perfiles equivalentes en otras plataformas y pruebas de red externa. Linux añade un adaptador propio Bubblewrap con mount/PID/network namespaces, filtro seccomp para sockets y ABI, handshake antes de argv, descriptores fijados y guard pidfd. Seis pruebas backend (una default y cinco OS opt-in ejecutadas) comprueban archivos hermanos/symlinks, escritura del runtime denegada, sockets IPv4/IPv6/UNIX/VSOCK e io_uring, canary FD cerrado, setup/cancel antes del payload y limpieza de descendientes setsid. Settings conserva perfiles bloqueados y exige el helper; la política aplica Deny si falta. Dos pruebas desktop opt-in y una default comprueban UI real 360/900, guardar/reabrir, actor stdin/EOF con lease, recibos y fallo sin helper sin ejecución Host. Linux 7.1.3 x86_64/Bubblewrap 0.11.2; ARM64, macOS, syscalls de publicación y proveedores reales siguen pendientes. Ver docs/linux-command-sandbox.md. |
| Comandos persistentes | Parcial | Gestor start/poll/stdin/terminate con límites, Job Object/ConPTY opt-in y MXC pipes en Windows; el catálogo legacy sigue disponible por digest para Resume. El desktop conserva un manager por dueño nativo, padre o hijo, en NativeCommandRegistry y lo revoca al retirar acceso o archivar/borrar. Controles humanos de stdin exacto, EOF, terminate y resize disponible usan NativeManualCommandActor bajo lease, permisos/hooks frescos y ledger propio; cinco cortes reales del actor y 27 cortes core de publicación prueban no repetición. Recibos y verificación de incertidumbre no fabrican ToolFinished. El reactor propio sostiene comandos entre runtimes: cinco pruebas Unix verifican Unicode/EOF, inicio sin runtime, revocación y Drop acotado. Linux offline comparte gate/guard Bubblewrap; cinco OS opt-in prueban pipes binarios entre turnos, archivos/red, hardlinks, cierre de árbol y SIGKILL real del owner con descendiente setsid. Un nuevo manager no restaura el proceso ni ejecuta otra vez. inspect_pending_output sigue siendo una copia del buffer que poll puede consumir, con aceptación independiente; no se presenta como historial. Ahora NativeCommandOutputStore captura por productor, con descriptor sincronizado antes de spawn, binding exacto dueño/workspace/generación/comando, ACK antes de notificar bytes, terminal tipado y páginas no consumidoras tras poll final/reopen. Conserva un prefijo acotado y rangos omitidos exactos; errores de publicación detienen el árbol sin mostrar bytes sin confirmar. Doce pruebas core incluyen doce SIGKILL en register/append/omission/finish × temp sync/rename/dir sync, quotas/corrupción y archive/delete/tree sin resurrección. Siete pruebas de proceso del productor, dos privadas y una nueva aceptación Linux offline ejecutada verifican ACK, fallo antes del payload, runtimes, binario, omisión y terminal. El runner HTTP/SSE real agrega dos cortes antes/después del ToolFinished de poll final, después de retirar el ID: reopen conserva bytes/terminal, el corte previo exige verificación manual, el posterior reanuda y el efecto ocurre una vez. Actividad → Historial de salida muestra páginas legibles por defecto, datos exactos opcionales, procedencia y diagnósticos completos incluso sin manager. Cinco aceptaciones App verifican procesos/UI, navegación/no consumo, scope/respuestas obsoletas e incompletitud a 360/900 px, repaint de App::ui real sin manager y journal truncado intacto; exports nativos inspeccionados. Ver docs/native-command-output.md, docs/native-command-operations.md y docs/persistent-commands.md. Quotas usan snapshots válidos cerca del límite y operaciones reales; no certifican un proceso emitiendo 8 MiB ni pérdida de energía/cortes dentro de syscall. Los workers hijos reciben un perfil propio del registro compartido: manager/captura por dueño, actor común con UI, catálogos Plan/legacy preservados y epoch que bloquea perfiles revocados. Nueve pruebas registry default, una Bubblewrap ejecutada explícitamente, seis worker, tres coordinador y una App HTTP/SSE comprueban runtimes separados, aislamiento padre/hermano, permisos/hooks, revocación del subárbol tras commit y controles bajo lease ocupado. Ver el bloque Comandos nativos por hijo. Faltan integración de automatizaciones con origen/política/aprobación propios, cortes adicionales en efectos de hijos, aceptación manual/captura ConPTY/MXC en Windows, ARM64/macOS y proveedores reales. |
| MCP, skills, hooks y extensiones | Parcial | Clientes stdio y Streamable HTTP, descubrimiento, catálogo, ajustes editables en UI y runtime conectado al runner Build. HTTP negocia 2026-07-28 o el ciclo de sesión de 2025; reanuda SSE legacy con `Last-Event-ID` sin repetir POST. Las notificaciones `tools/list_changed` en SSE de respuesta y GET legacy, y una renovación de sesión HTTP después de descubrir herramientas, invalidan el catálogo de forma persistente y bloquean llamadas futuras. Stdio sin credenciales y HTTP moderno HTTPS remoto, incluido bearer y OAuth en Windows con identidad ligada al vault, pueden fijar una huella acotada para resume; HTTP legacy y loopback siguen fail-closed. OAuth MCP tiene descubrimiento, PKCE, refresh, almacén en Windows Credential Manager y flujo conectado a Settings/runtime; faltan aceptación con servidor OAuth y vault reales. Marketplace activa sus skills instaladas; Settings permite elegir skills locales por workspace y la activación revalida el contenido antes de cada turno. Su digest de procedencia entra en las instrucciones y el marcador de contexto para resume. Hooks Pre/Post configurados en Settings tienen ejecución acotada, huella durable y recuperación conservadora de efectos inciertos; Plan excluye lecturas con hooks. La aceptación `process_cuts_reopen_plan_build_resume_and_checkpoint_without_repeating_effects` cubre ocho cortes reales Pre/Post en Unix (Started, efecto sincronizado con SIGKILL, diagnóstico, Finished), JSON de invocación, reapertura y ausencia de repetición; falta equivalencia en otros hosts y muerte dentro de syscalls. La proyección conserva HookStarted/HookFinished en el transcript y presenta ID, fase y decisión; las herramientas inciertas incluyen estos hooks en su tarjeta de verificación. `hook_activity_survives_store_reopen_and_manual_tool_resolution` verifica reapertura y que confirmar una herramienta no invente éxito del hook; `pending_hook_changes_from_live_to_uncertain_after_interruption` distingue ejecución de efecto incierto, y `hook_effect_details_and_verification_actions_remain_visible_on_small_surfaces` verifica los controles a 360/760 px. `HookDiagnosticCommitted` registra razón, stdout/stderr de hooks completados y fallo conocido de hooks inciertos antes de HookFinished, con límites e identidad verificados. Dos cortes adicionales tras diagnosticar preservan incertidumbre y no repiten efectos; journals anteriores sin diagnóstico conservan replay válido. La proyección y las tarjetas conservan estos campos al reabrir. Los lectores preservan stdout/stderr parcial con límite combinado ante timeout/cancelación, salida no exitosa y respuesta inválida; drenan cada pipe hasta dos segundos tras detener el árbol. Pruebas de procesos reales verifican ambos streams, límite agregado y reapertura del diagnóstico del runner sin confirmar el efecto. SIGKILL antes del append puede perder el diagnóstico y el drenaje no garantiza bytes posteriores al plazo. Faltan aceptación de la UI con un modelo real y más cobertura por plataforma. Faltan más notificaciones MCP y aceptación completa de extensiones. La inspección explícita en Settings realiza conexión/listado paginado/cierre stdio o HTTP contra fixtures reales y muestra metadatos completos, alcance, hash y fecha; no llama tools ni sustituye el catálogo congelado del turno. Cancelación/timeout durante initialize HTTP con session ID conocido intentan DELETE independiente y preservan tanto fallo original como fallo de cleanup. Runtime espera esa limpieza cooperativa; procesos muertos o futures descartados no garantizan cierre remoto. Preview de SKILL.md usa contenido revalidado en RAM y alta por hash exacto; borrar selección sólo del workspace exige snapshot/CAS, preserva otras preferencias y conflictos. OAuth/vault reales y las demás notificaciones siguen pendientes. |
| Instrucciones de proyecto | Parcial | `AGENTS.md` y `AGENTS.override.md` se cargan en orden desde la raíz Git/workspace con límites de niveles, archivos y bytes, y rechazo de enlaces en toda la cadena. La app inicia turnos con cwd en la raíz del workspace. Antes de pedir al modelo, un evento durable guarda versión, texto exacto y digest con límite de 512 KiB. Resume del padre e hijo usa esa copia aunque cambien los archivos. `read_file`, `write_file`, `list_files`, `search_files` y `apply_patch` detectan reglas anidadas del destino antes de operar, guardan la instantánea durable ligada a la llamada y la reproducen después de una caída o compactación. Comandos y herramientas externas aún no tienen ese preflight; faltan marcadores configurables y aceptación con proyectos reales. Memoria permite inspeccionar todas las copias adicionales iniciales `SystemInstructionsCommitted`, incluso anteriores al checkpoint, con texto íntegro, dueño, hash y número de evento. Cinco pruebas de proyección/UI y el ciclo Settings→HTTP→checkpoint→reopen verifican el contenido conservado y su presencia literal en la solicitud. Estas copias son instrucciones adicionales combinadas; no representan el wrapper completo de modo/tools ni todos los preflights anidados. La validación inicial ahora precede preparación MCP y controles humanos: tres aceptaciones reales del runner verifican límite exacto/sobrelímite, plan inválido, autoridad ajena y retry de admisión antigua; dos pruebas de frontera cubren enqueue/Started y mismatch de dueño. El host tiene otras tres pruebas con cuatro setups: AGENTS inválido UTF-8, sobrelímite, symlink Unix y skill seleccionado modificado. Reparar el mismo setup llega a inicialización/listado paginado MCP, streaming HTTP local y TurnCompleted, sin cambiar borrador/selección durante el rechazo. Ver el bloque de validación inicial y docs/project-instructions.md; no cuenta notificaciones, identidad MCP para Resume ni una cuenta real. |
| Subagentes y coordinación | Parcial | Hijos nativos durables con `spawn_id` idempotente, parentesco, límites, lease propio, ejecución concurrente, cancelación persistida y herramientas Build `spawn_agent`, `send_input`, `wait_agent`, `cancel_agent`. `send_input` usa una cola durable con reintentos idempotentes y recuperación tras varios puntos de interrupción. El control opaco deja avanzar el siguiente turno padre mientras el hijo corre; la app instala el wrapper, muestra sesiones hijas y recibe eventos persistidos mediante un bridge. Las herramientas hijas usan los permisos de escritura/comandos del usuario y presentan aprobaciones, cancelación y recuperación en la UI del padre o hijo; pueden crear descendientes con profundidad y cantidad acotadas. El archivado verifica descendientes no terminales y entradas pendientes; el borrado coordinado de la línea nativa usa intención durable y recuperación al iniciar la app. El snapshot atómico `list_children_snapshot_for_ui` y las tarjetas del framework propio muestran los seis estados, cancelación solicitada, profundidad y entradas pendientes también tras reabrir la app. `ui_snapshot_needs_no_parent_lease_and_does_not_mutate_registry`, `sidebar_projects_subagent_status_without_parent_lease` y `persisted_subagent_states_and_mailboxes_are_visible_without_a_worker` verifican reapertura, navegación padre/hijo restringida y layout a 360/900 px (Linux 2026-09-29). La acción UI persiste cancelación sin bridge para hijos Ready/Running y colas de hijos Completed, bajo lease del padre y validación de la línea seleccionada. `desktop_cancels_persisted_child_without_bridge_and_reopens_flags`, `desktop_cancellation_without_bridge_notifies_running_worker_durably` y `desktop_cancellation_closes_completed_mailbox_after_reopen` verifican acciones reales, reopen, worker flag, rechazo de otro target/padre ocupado y cierre de la cola sin alterar éxito histórico. `request_cancel_from_ui` rechaza estados interrumpidos/admisiones Ready en todo el subárbol bajo lock del registro; dos pruebas verifican rechazo sin cancelación parcial. Layout a 360/900 px verifica disponibilidad del control. El bridge activo conserva el control opaco del padre y persiste cancelación del subárbol antes de cerrar aprobaciones o señalar el token, incluso con otro lease padre activo. `bridge_registry_failure_keeps_approval_and_token_live_for_retry` verifica fallo del registro, aprobación/token sin cambios, retry y reopen; `bridge_without_durable_authority_cannot_signal_cancellation` verifica fail-closed, y el worker real de `cancelling_child_closes_approval_before_any_effect` verifica ausencia de efecto con padre ocupado. `bridge_cancellation_process_cut_reopens_subtree_before_notification` termina un subproceso real después del commit y antes de señalar el token; el reopen verifica leases liberados, cancelación del hijo/descendiente y entrada omitida. El fixture verifica coordinación sin inferencia ni journal hijo admitido. La UI despliega entradas Pending/Active/Done/Skipped con ID, modo y prompt exactos mediante snapshots acotados bajo el lock del registro, sin autoridad de edición/dispatch. `mailbox_snapshot_reopens_exact_prompts_and_all_lifecycle_states` verifica enqueue/claim/finish/cancel, Unicode multilínea, orden, reopen y lectura sin escritura; `persisted_mailbox_details_are_complete_and_scrollable` verifica presentación/scroll a 360/900 px y la prueba de acciones del sidebar rechaza otro target. La UI permite editar entradas Pending por revisión bajo lease del padre y lock del registro; conserva el digest original de `send_input`, rechaza entradas ya reclamadas/cerradas y mantiene el borrador ante fallo. Registro v3 lee v1/v2 y el borrado coordinado reconoce v3. `pending_edit_reopens_preserves_original_retry_and_blocks_claimed_changes` verifica reopen/retry/idempotencia/límites/autoridad; `pending_edit_and_claim_share_one_atomic_winner_across_handles` enfrenta editor y claimant en handles separados. El worker de `queued_follow_up_recovers_after_enqueue_claim_or_admission_crash` admite exactamente el prompt editado tras los tres cortes existentes. Dos pruebas UI verifican acciones, migración/reopen, conflictos sin perder borrador y controles a 360/900 px. La app despacha al reabrir colas de hijos Completed con identidad coincidente y replay Idle; un worker nativo propio reclama entradas mediante el coordinador, presenta eventos/permisos por bridge y conserva las instrucciones durables originales. `child_worker_request` elimina la restricción UI de una sola admisión y reconstruye el spawn desde la primera. `startup_dispatch_reopens_http_child_and_drains_frozen_mailbox_once` verifica HTTP local, host drop/reopen, despacho automático y retry UI tras modelo incorrecto sin modificar stores ni enviar petición; verifica mensaje de sistema idéntico, admisión única, entrada Done y reconstrucción tras dos admisiones. Otra prueba verifica diagnóstico/retry a 360/900 px. El fixture usa una cola Pending persistida y no credenciales externas. `competing_dispatchers_claim_one_reopened_input_and_keep_one_model_effect` enfrenta dos handles reales de despacho con stores reabiertos y un modelo local bloqueado durante el claim; verifica una sola llamada de seguimiento, una admisión y entrada Done tras reopen. Usa journal inicial real y cola persistida de fixture; no ejecuta dos procesos GUI completos ni un proveedor externo. `mailbox_publication_process_cuts_reopen_mutations_without_duplicate_turn` termina doce subprocesos reales sin destructores al publicar enqueue/edición/omisión/claim (temporal sincronizado, rename, directorio sincronizado). Reabre registro viejo/nuevo completo, verifica ausencia/retry de enqueue no publicado, revisión/prompt y Completed/Interrupted, exige Resume para claim publicado y completa con una sola llamada de seguimiento/admisión y Done tras otro reopen. Modelo local; no demuestra corte de energía ni Windows. `recover_registry_temporaries` valida el registro publicado y elimina temporales regulares con UUID v4 canónico bajo el lock del escritor; startup desktop y la siguiente escritura limpian huérfanos sin promoverlos. Pruebas de alcance/corrupción/symlink/lock, app reopen y los doce cortes verifican limpieza idempotente y bytes del registro sin cambios; snapshots siguen sin mutación. Windows y corte de energía quedan sin verificar. La omisión individual Pending usa `skip_pending_input` con revisión esperada y lease del padre, comparte lock con claim/edit, conserva prompt/digest y no cierra las otras entradas. `pending_skip_reopens_preserves_retries_and_only_removes_one_entry` verifica edición previa, stale/autoridad/retry/reopen y claim de la entrada retenida; `pending_skip_and_claim_share_one_atomic_winner_across_handles` enfrenta skip/claim en ocho carreras. `skipped_mailbox_input_is_never_admitted_or_sent_to_model_after_reopen` usa worker/modelo local real, reabre y verifica ausencia de admisión omitida y una sola inferencia retenida. La acción desktop incluye revisión, valida ámbito, reporta padre ocupado/conflicto, cierra el editor sólo tras éxito y presenta controles para Pending; pruebas de acción/reopen (cola fixture) y foco a 360/900 px verifican esos contratos. Los tres cortes de publicación de skip verifican Pending antes del rename y Skipped después, retry idempotente y cero llamadas/admisiones para la omisión publicada. Faltan compatibilidad de reservas Ready antiguas sin fuente exacta, aceptación con dos procesos GUI y cortes adicionales de proceso/efectos, edición con padre ocupado, cancelación coordinada con journals interrumpidos o padre ocupado sin bridge y aceptación con modelo real. `SubagentDispatchHandle` también admite reservas Ready con journal vacío e identidad exacta validada por validate_initial_child. Startup reconstruye prompt/modelo/base congelada desde el spawn_agent de la respuesta comprometida y ToolStarted del padre; reserved_spawn_request exige Build, argumentos estrictos y digest/version válidos de instrucciones. initial_ready_dispatch_reopens_validates_scope_and_claims_one_worker verifica reserva real, handles competidores, rechazos de prompt/modo/spawn, una admisión y una llamada al modelo local. La aceptación startup_dispatch_reopens_http_child_and_drains_frozen_mailbox_once añade Ready y modelo incorrecto tras reopen: ejecuta primero la reserva y después un mailbox fixture una vez, por HTTP local, preservando instrucciones frente a cambios de AGENTS.md. El prefijo del padre es fixture. `initial_child_dispatch_process_cuts_reopen_one_admission_and_exact_context` ejecuta cuatro cortes reales del worker (antes del claim, después del claim, después de admisión y después de TurnCompleted antes de cerrar el registro): reopen distingue Ready/Interrupted, un arranque normal no admite otra vez al interrumpido, Resume explícito conserva instrucciones y produce una admisión/una llamada local/Completed con lease liberado; en el cuarto corte reconcilia el registro sin otra llamada al modelo ni eventos nuevos de journal. Es instrumentación cfg(test) sin destructores; faltan cortes del host desktop completo, efectos posteriores, modelos externos y perfiles Ready antiguos cuya identidad no coincida. El registro de comandos compartido conecta Build/Resume/mailbox a un manager/captura por hijo, con actor UI idéntico, scope bajo lease y catálogos históricos intactos. Cancelación devuelve los IDs exactos del subárbol sólo después del commit, incluye descendientes terminales y bloquea nuevas reservas; tres pruebas coordinador, seis worker y una App HTTP/SSE verifican estas fronteras. La captura se lee tras reopen sin restaurar procesos. Ver el bloque Comandos nativos por hijo; cortes adicionales en efectos de hijos y modelos externos siguen pendientes. |
| Permisos por sesión | Parcial | El journal guarda de forma atómica la decisión y un grant exacto ligado al workspace, política, herramienta, argumentos y aislamiento. Runner, Resume y subagentes validan el grant antes de reutilizarlo; la UI ofrece «Permitir en esta sesión» para padre e hijo. La recuperación del hijo puede persistir el grant antes de ejecutar. Actividad muestra los grants de la sesión y permite revocarlos; la decisión histórica conserva la marca de revocación y las llamadas siguientes vuelven a pedir permiso. Hay pruebas de persistencia, reapertura y acción UI. Falta aceptación con proveedor real. |
| Intervenir durante un turno activo | Parcial | Una cola durable por turno registra instrucciones e imágenes con ID idempotente, valida lease y replay al confirmarlas, y un fence durable evita cerrar el turno con entradas pendientes. Runner y Resume las incorporan en límites seguros entre solicitudes y herramientas; hay pruebas de carrera y reapertura. La UI pone instrucciones en cola durante un turno nativo y conserva un nonce estable del borrador para reintentos ambiguos. Falta aceptación interactiva con proveedor real. |
| Automatizaciones nativas | Parcial | Origen ReadOnlyV1 y snapshot del host atómicos antes de admitir, vinculados al workspace y ocurrencia; sólo admite el primer pedido Build sin imágenes, steering o hijos. Creación entre handles, reapertura, corrupción sin reparación, límites, tombstone y tres cortes de publicación de metadata se verifican en native_automation_origin. DTO estricto conserva ruta/capacidades/límites/instrucciones y reconstruye Chat/Responses por HTTP local sin Settings actuales ni secretos serializados. Diez cortes reales del runner recuperan mediante ResumeWorker con una admisión, dos solicitudes SSE y una lectura; catorce mezclas de autoridad fallan sin preparación, requests o append. Dos aceptaciones App verifican despacho/retry y recuperación de metadata sin admisión→lectura→compactación→checkpoint/reopen con Settings actuales corruptos. Un symlink a otro workspace bloquea App/worker antes de efectos y conserva consulta histórica; no fija un descriptor contra cambios concurrentes posteriores. Ledger enlaza antes del worker y conserva el ID ante fallo. Ahora separa despacho de native_result V1 derivado de replay, checkpoint y perfil original bajo lease libre: Completed/Failed/Cancelled/Interrupted, terminal inmutable y prefijo monotónico. Se reconcilia tras caída, reapertura, Resume, compactación y resolución; journal/checkpoint inválidos preservan último resultado con diagnóstico por proyecto. Seis pruebas App con HTTP/SSE y dos cortes de proceso, cinco ledger, dos UI y dos del derivador verifican esas fronteras; lectura estricta conserva colas dañadas. Borradores admiten IDs de proveedores añadidos sin consultar claves/lista cerrada; despacho sigue validando registry y modelo exacto. UI presenta origen, recuperación, sólo lectura, detener, nueva tarea, resultado/despacho separados y Contexto compacto de la ruta original a 360/900 px. Comandos manuales, captura y acciones retrasadas de goals rechazan ese origen. Faltan efectos y sus permisos/aprobaciones por automatización, scheduler con múltiples procesos desktop, cuentas/modelos reales y aceptación de otras plataformas. El resultado tiene aceptación local; no certifica transacción entre stores, power loss ni latencia con journals grandes. Ver docs/native-automations.md y el bloque de aceptación final.  El DTO ReadOnlyV1 ahora congela y reconstruye también Messages, auth/headers y capacidad de imagen del perfil original. Campos nuevos opcionales se omiten en contratos anteriores para conservar digests; no se serializa la clave. Aceptación host Messages y límites se describe en docs/native-anthropic.md. |
| Entrada y salida multimodal | Parcial | La UI admite imágenes del proyecto con límites de 8 MiB por archivo, ocho por mensaje y 24 MiB agregados, y verifica capacidad por modelo. Core persiste bytes, MIME y hash en admisión o steering; replay/checkpoint los preservan, Runner/Resume envían la imagen exacta del journal, y Responses usa `input_image` con data URL. Chat Completions rechaza imágenes explícitamente. El conteo exacto del proveedor alimenta el presupuesto previo cuando hay ventana verificada; no se inventa una tarifa local por modelo. Pasan pruebas locales de transporte, corrupción y reapertura; faltan salida multimodal y aceptación con proveedor real.  Messages propio transporta las imágenes inline del journal; el host usa declaración image_input del ID exacto y preserva la negación explícita. El conteo directo incluye la misma imagen/herramientas, y la aceptación verifica reapertura y rechazo previo con límite de entrada menor. PDF/documents/audio/video, salida multimodal y cuenta real siguen pendientes. |
| Perfiles de aislamiento y herramientas alojadas | Parcial | El perfil Windows MXC cubre workspace offline. Responses permite búsqueda web alojada opt-in en modos caché, índice o vivo; conserva acciones, fuentes y citas en `provider_output_items`, vinculadas a la huella de petición para Resume. Settings activa el modo para OpenAI y la UI muestra citas clicables desde el journal. Faltan permisos diferenciados de lectura/denegación, perfiles aislados en otras plataformas y aceptación con proveedor real. El perfil Linux propio tiene evidencia separada de Windows y comparte los comandos de corta y larga duración con el motor. Sus montajes de runtime son limitados; home/rustup no se abren implícitamente. Aarch64 no se ejecutó y macOS continúa sin adaptador. Los hooks/MCP no heredan este perfil por presentarlo en Settings. Ver docs/linux-command-sandbox.md. |
| Reintentos del proveedor | Parcial | Chat Completions y Responses reintentan como máximo dos veces 429, 5xx, desconexiones y fallos de transporte antes del primer delta, con `Retry-After` acotado y cancelación. Tras el primer delta, un corte queda como error terminal para no duplicar texto. Pruebas de backend/Responses y Clippy pasan; falta aceptación con proveedor real. |
| Objetivos persistentes (goals) | Parcial | La app guarda texto por workspace/sesión en `composer-goals.json` y lo incluye en `SystemInstructionsCommitted` mediante `system_instructions_with_goal`; las pruebas `composer_goal_is_saved_per_draft_and_follows_the_created_session` y `goal_context_is_separate_from_visible_prompt_and_plugin_opens_real_catalog` verifican persistencia y composición. `NativeGoalStore` agrega un contrato core propio de ledger acotado por sesión, objetivo/criterios inmutables, Active/Paused/Complete, lease nativo + lock entre handles, revisiones y operaciones idempotentes, historial y publicación atómica. `RecordEvidence` exige un TurnCompleted de la misma sesión y Complete exige evidencia para todos los criterios y estado Active. Tres pruebas core verifican reapertura, reintentos, corrupción/criterios, autoridad y carrera de revisión; `native_goal_completion_requires_all_evidence_from_real_completed_turn_and_survives_reopen` ejecuta un AgentRunner real con modelo local, rechaza cierre prematuro/evidencia desconocida/pausa, verifica cierre/reopen/historial y borrado coordinado. La asociación de evidencia a un turno exitoso no demuestra satisfacción semántica del criterio. `goal_publication_process_cuts_reopen_exact_state_and_retry_one_revision` termina quince subprocesos al publicar creación/pausa/resume/evidencia/cierre en tres fronteras; verifica ledger anterior/nuevo completo, leases liberados, reintento con una revisión y journal intacto. Evidencia/cierre usan markers de fixture core, sin inferencia en esos cortes. `recover_temporaries` y los mutadores limpian sólo temporales regulares UUID v4 de la sesión bajo lease/lock; los cortes y otra prueba verifican ausencia/bytes intactos, idempotencia, corrupción, sesión ajena, directorios/symlinks. Linux; no power loss ni Windows. Ver contrato/límites en `docs/native-goals.md`. `recover_idle_sessions` inventaría sólo archivos de goals y toma leases no bloqueantes; startup desktop conserva sesiones ocupadas, reporta corrupción/missing metadata/no-nativas por sesión y continúa con las válidas. Dos pruebas core/desktop verifican casos mixtos, cleanup/reopen/retry, journal admitido sin cambios y warning renderizado; el informe completo conserva IDs/razones. La fila de sesiones sigue siendo breve; el editor Goal muestra el informe global íntegro en native_goal_recovery_details, con IDs ocupados/diagnósticos sin recortar y retry explícito native_goal_maintenance_retry. La prueba startup ampliada verifica bloqueo durante ejecución/fuera del editor, conservación al cambiar selección, retry ocupado/corrupto y limpieza de dos temporales tras liberar/reparar, sin cambiar ledger/journal. native_goal_maintenance_exposes_full_report_and_retry_in_own_ui verifica diagnóstico largo íntegro, scroll y pointer/disabled a 420/900 px. No cuenta reparación del fixture como reparación automática del producto. `NativeGoalToolExecutor` agrega get_goal en Plan y consulta/evidencia/cierre en Build, bajo admisión/lease/workspace actuales; no crea objetivos ni expone pausa/resume. Dos pruebas del adapter rechazan scopes/modos/args/cancelación/autoridad obsoleta sin mutación y distinguen operación por turno+call ID. `native_goal_tools_run_serially_in_real_turn_and_reopen_completed_criteria` ejecuta seed y turno con batch de tres tools a través de runners/modelos locales, verifica objetivo íntegro en contexto, tres resultados, revisión 2 Complete y reopen. `committed_update`/`committed_mutation` inspeccionan recibos de argumentos exactos sin modificar ledger/journal, distinguen revisión aceptada/actual y rechazan corrupción/conflicto/scope. `goal_tool_commit_cuts_reopen_exact_receipts_without_repeating_mutation` ejecuta dos cortes de proceso con runner/modelo local después del guardado de evidencia/cierre y antes de ToolFinished; reapertura verifica recibo, resultado faltante, retry sin segunda modificación y journal intacto. Los doce cortes de publicación de mutaciones también verifican recibo presente/ausente antes del retry. El adapter está instalado después de admisión del padre desktop y en Resume antes de preparar MCP; `certifies_plan_read` permite get_goal serial en Plan sin certificar concurrencia. `plan_runner_reads_native_goal_without_advertising_mutations_or_changing_ledger` verifica un runner/modelo local real con getter, objetivo íntegro, dos llamadas y ledger intacto. `resume_installs_goal_reader_and_preserves_frozen_plan_catalog_after_store_reopen` verifica worker desktop con dos llamadas/getter tras reapertura de un prefix de journal fixture; `frozen_catalog_before_goals_blocks_new_goal_tools` conserva el digest anterior y rechaza llamadas ocultas. Wrappers de recuperación nuevos pueden servir admisiones fallidas/canceladas bajo host confiable después de replay/Resume explícito; los wrappers normales quedan revocados y ninguna admisión completada recupera autoridad. Prueba adicional verifica revocación/autoridad fresca/turno posterior. `SubagentWorker::with_native_goals` está activado en workers desktop y construye wrappers nuevos por lease/admisión del hijo, con schemas en la identidad de reserva; no hereda ledger/autoridad del padre. Tres pruebas verifican ejecución real Plan/Build al límite de profundidad (rechazo de mutar goal del padre/citar turno completado del padre), recuperación de hijo con goals y recuperación de reserva anterior sin agregar herramientas. Dos prefijos de recuperación son fixtures de journal; los modelos/tools se ejecutan realmente y se verifica catálogo/ledger al reabrir. El editor existente agrega activación nativa explícita con objetivo completo/criterios por línea y controles de pausa/resume ligados a sesión/goal/revisión, bajo lease real; no migra ni reescribe composer-goals.json. Tres pruebas verifican acciones reales de activar/pausar/resume/reopen, criterio vacío/busy/lease externo/selección ajena/CAS externo/corrupción y layout/pointer en openide-ui a 420/900 px con ID de control exacto. Proyecta status/revisión/criterios/evidencia; drafts de criterios son sólo memoria. El padre y los hijos vinculan una proyección completa de goal/criterios a SystemInstructionsCommitted y al digest antes de inferencia; el padre valida el contexto bajo lease antes de admitir el turno. Dos pruebas de codec verifican Unicode/base exacta/status/revisión/sin historial y rechazo de overflow 512 KiB sin recortar criterio alguno. El Plan runner verifica presencia en la primera solicitud; Resume conserva el snapshot original frente a pausa posterior del ledger/cambio de instrucciones. El worker hijo reconstruido desde el envelope verifica el digest original y ejecuta un follow-up real sin duplicar contexto. `native_task_instructions` selecciona el ledger Active/Paused/Complete sobre texto anterior del composer: base exacta del proyecto para el hijo, objetivo nativo completo para el padre y texto legacy preservado sin reactivación implícita. Una prueba comprueba tres estados/bytes/fallback; la prueba de activación UI conserva texto anterior pero muestra el objetivo nativo. `resume_before_context_commit_selects_native_goal_under_lease_and_preserves_legacy_fallback` verifica dos prefijos admitidos fixture, selección bajo lease, inferencia local real, instrucciones persistidas y ledgers intactos. Después de un marcador de contexto sólo se usa el snapshot/digest original. `process_cuts_reopen_plan_build_resume_and_checkpoint_without_repeating_effects` verifica ahora objetivo y criterio completos por HTTP local en 22 cortes reales (incluidos SystemInstructionsCommitted y ModelContextCommitted), dos snapshots exactos Plan/Build, ledger revision 0 sin evidencia inventada tras Resume y checkpoint/reapertura, un siguiente Plan explícito con solicitud HTTP que conserva el goal completo sobre el historial resumido, y efecto único; los hooks mantienen sus recibos sin repetirse. La prueba HTTP sin muerte real verifica además pausa posterior al snapshot y Plan explícito tras checkpoint con goal Paused íntegro. ResumeWorker ahora inspecciona recibos exactos de record_goal_evidence/complete_goal y entrega Found/NotFound/Failed a la tarjeta de verificación; muestra goal/operación/revisión guardada/actual en scroll y conserva la espera manual sin ToolFinished sintético. `native_goal_receipt_recovery_reports_exact_state_without_replaying_or_publishing_result` verifica reapertura, mutación real y tres estados (evidencia completada por journal fixture), ledger/journal intactos durante espera y cancelación de ausencia/corrupción, verificación manual de recibo encontrado seguida de un único ToolFinished/modelo local/TurnCompleted sin repetir mutación y lease liberado. `native_goal_receipt_details_preserve_manual_verification_controls` verifica contenido y controles a 420/900 px. Falta reconciliar el resultado original de manera demostrable y aceptar cortes reales desktop/UI de ese recorrido. Faltan otras rutas, activación desde draft nuevo, presentación completa a todos los tamaños/accesibilidad y aceptación integral UI→modelo→evidencia→checkpoint; el gate de incertidumbre sigue vigente. Faltan cierre integral de composer/UI y otras rutas host, continuación autónoma, blocked audits, presupuestos/uso, recuperación de turnos/checkpoints y aceptación de plataformas. |
| UI y Settings | Parcial | Agente presenta el harness propio y el perfil nativo guardado, muestra proveedor/ID exactos y estado de credencial sin anunciar aceptación de generación. El switch de escritura/comandos conserva las lecturas independientes y el aislamiento consulta el mismo gate de plataforma del motor; un perfil guardado no disponible permanece bloqueado sin fallback. Dos pruebas `settings_agent_*` verifican scopes/acciones y superficies a 360/900 px. Memoria distingue contexto estimado, instrucciones, ledger de goals y memoria aprendida pendiente. `settings_native_actions_recheck_owner_execution_and_recovery_before_leaving_settings` verifica dueño durable, CLI, selección ausente, ejecución y recuperación sin admisiones ni efectos; la apertura del editor nativo desde Settings se comprueba en `native_goal_editor_activates_exact_criteria_pause_resume_and_reopens`. `settings_memory_compacts_real_http_turn_and_reopens_checkpoint_and_goal` realiza Plan por HTTP local y clic UI→compactación sin herramientas→checkpoint durable→reapertura, preservando objetivo/criterios sin inventar evidencia. Settings Subagentes reutiliza permisos, Resume, cancelación y mailbox del motor; tres pruebas `settings_subagents_*` cubren los seis estados, scope, navegación, edición/omisión por revisión y pointer a 360/900 px, incluido inicio explícito de Ready. La vista previa de reglas usa el archivo guardado fresco y el evaluador real sin ejecutar comandos; cinco pruebas de preview/reapertura verifican cambios externos, corrupción, contenido íntegro y ausencia de efectos. Faltan aceptación integral de todas las superficies con modelos reales, accesibilidad/foco y recuperación completa por plataforma; la compilación no cierra la capacidad. ContextMeter/ContextContent y Marker/TextShimmer son componentes Rust propios de `openide-ui`. Contexto tiene scroll y desglose por secciones; unknown se distingue de cero. El selector de motor externo se retira: nuevas tareas usan el harness nativo; el historial CLI conserva su origen y ofrece Nueva tarea nativa, sin mutar su journal ni continuar una CLI. El selector de modelos muestra sólo conexiones disponibles; los proveedores sin conectar se gestionan en Settings. Marcadores de compactación completada requieren CheckpointCommitted; progreso manual exige worker activo. Git presenta cambios observados entre dos revisiones reales del mismo ámbito, conservados sólo en RAM. No hay aún evento de comienzo de compactación automática ni histórico durable de cambio de rama. Ver `docs/conversation-markers.md` y `docs/settings-native-ux.md`. La aceptación host de instrucciones inválidas conserva el borrador RAM y reabierto, la selección y ausencia de sesión/admisión/workers; la reparación ejecuta el mismo motor y transportes locales reales. La prueba no certifica una ventana GUI en otra plataforma ni una cuenta de generación externa. Historial de salida conecta el store durable del productor sin necesitar manager tras reopen, con paginación legible, base64 para segmentos binarios, JSON exacto opcional y detalle íntegro de argv/procedencia/errores. Cinco aceptaciones App de proceso/UI verifican controles y scroll a 360/900 px, guards de respuestas, repaint real y journal truncado intacto; el host programa repaint durante lecturas pendientes aun sin turno activo. Contexto inicial tiene máximo 320 × 200 px, barra fina de entrada/salida y detalles separados; sin conexión muestra Sin modelos/No hay modelos disponibles y deshabilita envío. Conectar proveedor permanece en todas las páginas del selector y abre Settings sin cambiar borrador o sesión. Activity y controles usan el mismo registro/actor para hijos. La aceptación App HTTP/SSE comprueba aprobación, stdin exacto/EOF bajo lease hijo ocupado, follow-up con worker reconstruido, final poll, cancelación sin bridge y captura tras reopen a 360/900 px; no certifica todos los cortes ni cuentas reales. Hovers de los grupos de proveedores respetan las esquinas superiores/inferiores, incluido filtrado a una fila: seis pruebas de acciones/pintura a 360/900 px y raster con alfa verifican ausencia de uniones. CornerRadii es propio del framework para fondos/bordes sólidos y foco; no introduce clipping redondo general de descendientes. El filtro visual de cuatro conexiones conserva la ruta/esfuerzo durable del motor, y la selección reordena la fila activa antes de restaurar el foco del modal. La confirmación de skills conserva contenido y controles con scroll a 360/900 px y alturas 320/720. |

## Auditoría y verificación en Linux (2026-09-29)

Base clonada: `codex/ui-component-quality` en `bad325e`, árbol inicialmente limpio.
Referencia de lectura: `references/codex` fijada en
`33a0f766a647208b471cfbcad889c67fd324ee04`. Los contratos consultados incluyen
`core/src/context/user_goal.rs`, `protocol/src/protocol.rs` (`ThreadGoal`),
`core/src/agent/control/resume.rs` y `core/src/compact_remote_v2_attempt.rs`.
El estado de objetivos del protocolo de referencia confirma una diferencia con
el texto de objetivo que actualmente conserva el composer de OpenIDE.

Con Rust 1.95.0 y dependencias GCC/pkg-config/Wayland/libxkbcommon provistas por
Nix, una corrida completa anterior de `cargo test --workspace` pasó **1229
pruebas, con 14 ignoradas, en 12 suites**, con concurrencia predeterminada
(2026-09-29). Una corrida anterior
con `--test-threads=4` también pasó; no se repitió para estas últimas adiciones. `cargo fmt --all -- --check` y `git diff --check`
pasaron. `cargo clippy --workspace --all-targets` terminó sin errores, con 46
advertencias de estilo en `agents-window` (45 `collapsible_match` y una
`nonminimal_bool`); la variante con `-D warnings` no pasa todavía. Los helpers
de interrupción están ignorados y se ejecutan explícitamente desde sus tests
padre; las pruebas externas de OpenAI/DeepSeek también permanecen ignoradas.
La nueva prueba externa de catálogo público OpenRouter está ignorada por defecto,
pero pasó al ejecutarse explícitamente; sólo demuestra metadatos y cache. No se
contaron ejecuciones omitidas por falta de claves como aceptación de inferencia real.

Durante la integración de omisión individual y el trabajo visual compartido,
una corrida intermedia falló en cinco pruebas de Settings (frame, catálogo,
MCP y OAuth). La nueva corrida completa posterior a la integración pasó las
1229 pruebas indicadas arriba; esa corrida reemplaza la observación intermedia
como evidencia del estado actual. Las pruebas de temporales ahora cubren
reapertura, namespace/tipo de archivo, corrupción, lock de escritor activo y
los doce cortes reales de publicación. La suite verde sigue sin demostrar los
criterios funcionales pendientes en la tabla.

La integración posterior de goals añadió tres pruebas core y una prueba de
runner nativo (modelo local) en `tests/native_goals.rs`; core/harness pasaron
501 pruebas y dejaron 10 ignoradas al verificarse por separado. La nueva corrida
completa arriba también pasó después de resolver errores de integración de los
componentes visuales en edición y separar el ID de tooltip del ID de selección
del picker: una ruta Unsupported no adquiere una región de selección por su
tooltip. Este resultado reemplaza las fallas intermedias observadas durante
ediciones concurrentes. El contrato/límites de goals está documentado en
`docs/native-goals.md` y aún no incluye el ciclo autónomo ni su integración UI.

La verificación posterior agrega quince cortes de publicación de goals y
limpieza de sus temporales por sesión, bajo lease/lock, sin promover candidatos.
Los casos de evidencia/cierre en esos cortes usan un journal fixture core y no
se cuentan como inferencia. La prueba con runner/modelo local sigue separada.
La corrida integrada actual incluye trabajo visual concurrente; no se atribuye
su crecimiento completo al harness. La API pública de tiempo de layout permite
que los consumidores del framework prueben gráficos/tooltips con reloj fijo.

La recuperación de goals al inicio ya está conectada: inventario de archivos
propios, leases no bloqueantes, diagnósticos independientes y warning de la
observación de inicio que conserva IDs/razones en el reporte. La prueba desktop
reabre con goals válido/ocupado/corrupto y una admisión pendiente; conserva el
journal y los datos no recuperables, presenta el warning y completa limpieza al
reabrir después de liberar/reparar. La API `Frame::text_layouts` permite verificar
fuente/geometry/clipping desde consumidores del framework sin leer sus campos
privados. El editor Goal ahora presenta detalles completos y retry de mantenimiento, verificados según la tabla. Continúan pendientes ciclo autónomo y aceptación integral UI/plataformas.

La auditoría corrigió la conversión de errores I/O envueltos por serde_json en
el store y las expectativas de pruebas sobre disponibilidad PTY y scroll OAuth
en Linux. Las pruebas de hooks con límites de tiempo fallaron bajo concurrencia
predeterminada antes de corregir sus plazos de arranque. Ahora esperan el marcador
real antes de cancelar, y las pruebas de timeout dan cinco segundos de arranque
mientras el hook duerme treinta segundos después de su efecto; conservan la
exigencia de observar ese efecto y su clasificación incierta. La suite con
concurrencia predeterminada pasó después de esos cambios.
Estos resultados no demuestran aislamiento Linux, OAuth con vault Windows,
salida multimodal ni ejecución de modelos reales. Todas las capacidades de la
tabla permanecen parciales y el objetivo sigue abierto.

## Orden de implementación

1. Hacer durable la admisión y el log: ID de turno, ACK, eventos idempotentes,
   replay y recuperación del borrador. Esta es la base para que la UI y el
   runtime acuerden qué trabajo existe.
2. Establecer el protocolo de modelo y el modo por turno: Responses, uso real,
   Plan/Build y una matriz explícita de capacidades por proveedor.
3. Implementar compactación y resume con checkpoints, control de presupuesto
   y pruebas de interrupción en cada punto de efecto externo.
4. Completar herramientas: registro, validación, scheduler, sandbox por
   plataforma, MCP y políticas de permisos por sesión/proyecto.
5. Añadir subagentes, skills/hooks y proyección completa a la UI propia.
   Pasar una aceptación de punta a punta en Windows, incluidas sesiones
   recuperadas y modelos reales compatibles.

El objetivo permanece abierto hasta que todos los criterios estén satisfechos.
Cada fase debe actualizar esta tabla con evidencia de pruebas y limitaciones
reales; una compilación verde por sí sola no demuestra paridad con Codex.

## Mapa del clon de Codex

Estas rutas del commit fijado guían la implementación original en Rust:

| Área | Fuente de lectura en `references/codex/codex-rs/` |
| --- | --- |
| Turno y muestreo | `core/src/session/turn.rs`, `core/src/tasks/regular.rs` |
| Plan y modo normal | `protocol/src/config_types.rs`, `collaboration-mode-templates/templates/` |
| Compactación local, remota y automática | `core/src/compact.rs`, `core/src/compact_remote_v2.rs`, `core/src/tasks/compact.rs`, `core/src/session/turn.rs` |
| Reanudar y reconstruir historial | `core/src/thread_manager.rs`, `core/src/session/rollout_reconstruction.rs`, `thread-store/src/`, `rollout/src/recorder.rs` |
| Herramientas y permisos | `core/src/tools/{registry,router,orchestrator,sandboxing}.rs`, `protocol/src/approvals.rs` |
| Protocolo para una UI externa | `app-server-protocol/src/protocol/common.rs`, `app-server-protocol/src/protocol/v2/` |
| Modelos y proveedores | `model-provider-info/src/lib.rs`, `model-provider/src/provider.rs`, `models-manager/src/` |

El motor local de Codex usa Responses como protocolo de proveedores; su
catálogo de modelos contiene atributos propios de Codex. OpenIDE mantiene
por separado los adaptadores y sus comprobaciones de compatibilidad. Copiar
solo el loop de turnos dejaría fuera el rollout, la recuperación, el
aislamiento y el flujo de aprobaciones.

Los límites de contexto para el adaptador OpenAI se comprobaron en las fichas
oficiales de [GPT-4.1](https://developers.openai.com/api/docs/models/gpt-4.1),
[GPT-4o](https://developers.openai.com/api/docs/models/gpt-4o),
[GPT-5](https://developers.openai.com/api/docs/models/gpt-5),
[GPT-5.4](https://developers.openai.com/api/docs/models/gpt-5.4) y
[GPT-6](https://developers.openai.com/api/docs/models). El presupuesto reserva
la salida máxima documentada mientras el adaptador no fije un máximo menor
por solicitud.

La aceptación con el adaptador nativo de OpenAI requiere una credencial de la
[API de OpenAI](https://developers.openai.com/api/docs/quickstart) disponible
para este proceso. El inicio de sesión de
[Codex CLI](https://learn.chatgpt.com/docs/developer-commands#codex-login)
puede usar OAuth de ChatGPT o una clave API, pero su sesión no autentica
automáticamente las solicitudes Responses del harness Rust.

Verificación más reciente durante edición visual compartida: el nuevo selector
de objetivo nativo/legacy y Resume antes del commit de contexto pasaron junto
con 73 pruebas de `agents-window` filtradas por `native_`. `cargo test --workspace`
falló en `disconnected_provider_shows_one_connection_action_instead_of_models`
(514 pasaron, 1 falló, 2 ignoradas en la suite de app); el encabezado de cuenta
que había fallado en una corrida intermedia ya pasó. La ejecución completa se
detuvo en esa suite, así que no se presenta como verificación de todo el
workspace. Clippy terminó sin errores con 47 advertencias (45 collapsible_match,
1 nonminimal_bool en la app y 1 too_many_arguments en el tooltip del framework).
`cargo fmt --all` se ejecutó; el check posterior detectó nuevos cambios sin
formatear en Settings por la edición concurrente. Diff check pasó. Revalidar
las puertas integradas al estabilizar el trabajo visual; ningún criterio
funcional se cierra por compilación.

Verificación final de formato de este bloque: después de aplicar fmt de nuevo,
`cargo fmt --all -- --check` y `git diff --check` pasaron sobre el worktree
observado. No se repitió la suite completa sólo por cambios de formato.

Aceptación adicional del contexto de goals: la prueba HTTP local
`plan_build_uncertain_effect_reopens_resumes_and_replays_checkpoint` pasó
el 2026-09-29 con el objetivo y ambos criterios íntegros en Plan/Build. Resume
conserva el snapshot Active aunque el ledger se haya pausado; tras compactación,
checkpoint y reapertura, un Plan explícito nuevo recibe el goal Paused íntegro
y el historial resumido. El efecto de archivo sigue siendo único. Este caso
simula la ausencia del marcador terminal: no añade un corte real de proceso
ni verifica cierre de criterios, ciclo autónomo, UI integral o proveedor externo.
Las capacidades correspondientes siguen parciales.

La suite completa de openide-harness pasó 414 pruebas, con 11 ignoradas,
en 5 suites. Después se añadió la solicitud del siguiente Plan explícito tras
checkpoint/reapertura en cada corte: la suite native_http_cycle pasó 4 pruebas,
con 2 helpers ignorados en la enumeración normal (el padre los invoca). Ese Plan
recibe por HTTP el goal completo y el historial resumido; ledger y efecto quedan
sin nuevas modificaciones. Workspace sigue detenido por la prueba visual indicada.
Clippy terminó sin errores, con 47 advertencias; fmt/diff check pasaron.

Última observación del worktree compartido: nuevas metadata de modelos agregaron
`description`/`input_modalities` a DiscoveredCodexModel. Se adaptaron los dos
fixtures de app/composer_selection que no inicializaban esos campos; la nueva
corrida de Clippy pasó sin errores, con 53 advertencias (incluye metadata aún
sin consumidores durante la edición visual). Fmt/diff check pasaron. Este
resultado reemplaza el conteo anterior de 47 advertencias; no se repitió todo
workspace tras estas adaptaciones mecánicas. La falla visual de la última suite
workspace permanece una observación pendiente de nueva verificación.

Verificación de integración de recibos: 13 pruebas de ResumeWorker pasaron;
las dos pruebas nuevas de recibo/controles UI también pasaron. La continuación
del caso guardado usa salida escrita explícitamente por el verificador manual,
registra un único ToolFinished, completa con modelo local y conserva el ledger.
Workspace sigue fallando en
`disconnected_provider_shows_one_connection_action_instead_of_models`
(516 passed, 1 failed, 2 ignored de app); no se atribuye esa corrida a aceptación
completa. Clippy terminó sin errores, con 48 advertencias; fmt/diff check pasaron.
El cierre original de evidencia del fixture es core, sin inferencia; la recuperación
desktop y su siguiente llamada al modelo se ejecutan realmente. Faltan cortes
reales desktop/UI y proveedores externos para esta ruta.

Verificación del arranque inicial de reservas Ready: la aceptación HTTP startup
pasó cuatro escenarios y la suite de dispatch handles pasó dos pruebas; la
prueba de fuente del spawn también pasó. openide-harness completo pasó
415 pruebas, con 11 ignoradas, en 5 suites. Workspace se detuvo en la misma
prueba del picker `disconnected_provider_shows_one_connection_action_instead_of_models`
(518 passed, 1 failed, 2 ignored en app); no es una corrida verde de todo el
workspace. Clippy terminó sin errores y con 47 advertencias. Fmt/diff check pasaron.
No se cuentan los fixtures de journal/mailbox como inferencia ni como cortes
reales del desktop. Los demás criterios pendientes del roadmap siguen abiertos.

Verificación de cortes del arranque/cierre inicial: openide-harness pasó
416 pruebas, con 12 ignoradas (5 suites), antes de ampliar el cuarto corte;
la prueba enfocada ampliada pasó los cuatro cortes. Los helpers ignorados se
ejecutan desde sus pruebas padre. Clippy terminó sin errores y con 47
advertencias; fmt/diff check pasaron. Workspace continúa fallando en
`disconnected_provider_shows_one_connection_action_instead_of_models`
(518 passed, 1 failed, 2 ignored en app), por lo que no se presenta como
aceptación verde de todo workspace.

El cuarto corte ocurre después de TurnCompleted y antes de finish_child: tras
reopen el registro proyecta Interrupted, y Resume explícito reconcilia Completed
sin otra petición al modelo ni eventos nuevos de journal. Son cortes reales
del worker, independientes de la aceptación HTTP desktop con fixtures del padre.
No demuestran cortes dentro de syscalls, efectos de herramientas posteriores,
dos procesos GUI completos ni proveedor externo. Los criterios restantes siguen abiertos.


## Integración de Settings, Contexto y marcadores (2026-09-29)

La corrida completa posterior a esta integración pasó **1380 pruebas, 20
ignoradas, en 12 suites**, con concurrencia predeterminada y el entorno GCC /
pkg-config / Wayland / libxkbcommon de Nix. Incluye el nuevo evento de identidad
de ruta, sus cortes reales de proceso, inspectores de instrucciones y salida
pendiente, MCP, Settings y el Contexto compacto. Los fixtures de proceso se
siguen ejecutando desde sus pruebas padre; las pruebas externas ignoradas no se
cuentan como generación real.

La corrida anterior se detuvo en seis pruebas (cuatro contratos viejos del
selector CLI, Cancelar de OAuth fuera de bounds y una carrera del fixture de
cancelación MCP). Se corrigieron los contratos nativos, la geometría y la
barrera de cancelación; sus suites focused pasaron. Otro gate intermedio detectó
un assert del color antiguo del aviso de error, sustituido por el acento real del
componente Alert con copia conservada. La corrida completa verde reemplaza esos
resultados intermedios. Una colisión durante edición compartida de Alert se
resolvió recuperando el archivo original completo; no se sustituyó su API por un
stub.

`cargo clippy --workspace --all-targets` terminó sin errores y con **66
advertencias**: 62 collapsible_match, dos diferencias de tamaño entre variantes,
un helper sin uso y un unwrap después de is_some. No es un gate de -D warnings.
`cargo fmt --all` se aplicó, `cargo fmt --all -- --check` y
`git diff --check` pasaron después de los últimos ajustes.

El Contexto inicial de este bloque se rehízo como resumen compacto:
porcentaje/cociente, barra de entrada/salida reportada, siete categorías con
unidades y acceso explícito a detalles. Fue reducido nuevamente el 2026-09-30
según se describe abajo. Las fracciones reales y acciones se verifican en
openide-ui; dos pruebas host verifican presentación compacta/detalles, ausencia
de datos técnicos en la vista inicial y scroll/teclado a 320/620 px. El fixture
HTTP exportó y se revisaron PNG a 420/900 px para uso inicial, checkpoint sin
medición y refill tras reopen. El resultado no atribuye tokens a los bytes de
skills/AGENTS ni convierte el catálogo congelado (sólo digest) en un inventario
de esquemas. La pantalla conserva esos límites; el goal y las capacidades de la
tabla siguen pendientes de aceptación integral.

## Contexto compacto, selección conectada y streaming (2026-09-30)

La tarjeta inicial de Contexto mide como máximo 320 × 200 px lógicos, incluidos
los márgenes del popover. Muestra porcentaje/cociente, barra fina y dos filas:
entrada y salida. Las siete categorías y la procedencia siguen en Ver detalles,
con su scroll separado. Se revisaron exports PNG reales del fixture HTTP a
420/900 px: uso, checkpoint sin medición y refill tras reopen. Esto verifica
geometría/pintura del árbol, no equivalencia de píxeles con egui/wgpu.

El selector vacío no muestra el modelo predeterminado ni esfuerzos obsoletos.
Tiene «No hay modelos disponibles» («Sin modelos» en el chip estrecho), Enviar
deshabilitado y «Conectar proveedor» permanente en las tres vistas del picker.
El clic real abre Proveedores → Conexión sin cambiar ruta, preferencias,
borrador o store. El runner rechaza el perfil implícito antes de preparar
adjuntos o admitir un turno; una prueba en subproceso sin las tres variables
OPENIDE verifica esa frontera sin poder alcanzar un endpoint público.
Las rutas descubiertas sin clave consultan `key_optional` y el resolvedor del
adaptador registrado. No se pierden al insertar el par exacto descubierto;
ninguna lista cerrada de proveedores decide su disponibilidad.
Settings → Agente también oculta el modelo implícito sin candidato exacto,
conserva rutas explícitas y estados negativos y conecta su botón al mismo flujo.

Las pruebas locales de streaming ahora comprueban primera pintura antes del
terminal, UTF-8 dividido, scroll, Stop real, tail tras fallo/cancelación,
reapertura y commit autoritativo sin display redundante. Resume conserva
ModelInFlight ante respuesta no confirmada y continúa sólo por acción explícita.
La regresión `plan_runner_and_resume_commit_cut_preserve_text_and_structured_card_without_duplicates`
usa runner, stores y Resume reales con cortes controlados tras commit/propuesta:
texto y tarjeta Plan sobreviven, sin otra llamada al modelo, a 360/900 px.
Ver `docs/native-streaming.md` para contratos, pruebas y límites de los adaptadores.

`native_http_cycle` pasa cinco pruebas y mantiene tres helpers ignorados que
sus padres ejecutan en procesos separados. Incluye dos nuevos cortes durante
Build activo después de compactar el prefijo Plan; la matriz general comprende
catorce fronteras básicas y ocho de hooks en Unix. Los tres cortes de publicación
de checkpoints siguen siendo una aceptación distinta. El presupuesto de la nueva
fixture es explícito, no un límite anunciado por un proveedor real. No se cubren
muerte dentro de syscalls, pérdida de energía ni todos los efectos externos.

Las capacidades de la tabla siguen Parcial: los cierres locales no sustituyen
aceptación con credenciales, otras plataformas, salida multimodal, controles
manuales de comandos persistentes ni los demás criterios pendientes del goal.

Gate integrado final de este bloque: `cargo test --workspace` pasó **1438
pruebas, 23 ignoradas, en 12 suites (20,41 s)**, con concurrencia predeterminada.
Incluye Settings → Agente sin fallback, navegación del selector, streaming y
los dos cortes de checkpoint activo. Las ignoradas incluyen helpers ejecutados
por sus padres y aceptación externa opt-in; no cuentan como inferencia externa.
`cargo fmt --all`, su check final, `git diff --check` y el build de
`agents-window` pasaron. `cargo clippy --workspace --all-targets` terminó con
**0 errores y 67 advertencias**: 63 collapsible_match, dos variantes de distinto
tamaño, un collapsible_if y un unwrap tras is_some, todos en el desktop. El gate
estricto de `openide-harness --all-targets -- -D warnings` también pasó.


## Controles humanos de procesos: integración y recuperación (2026-09-30)

El control manual ya tiene autoridad propia, independiente del journal del modelo.
`NativeManualCommandActor` consume intenciones confirmadas bajo el lease nativo
actual; el host carga permisos, reglas argv y hooks guardados frescos. Antes de
cada hook vuelve a comprobar la misma política y registry aprobados, sin adoptar
un cambio de configuración durante la operación. Una observación de stdin/EOF/
resize/terminate se sincroniza antes de post-hooks y conserva su resultado aunque
un post-hook falle. El input de la UI conserva bytes CRLF/CR, Unicode y NUL; la
confirmación no agrega un salto de línea.

La admisión, `ModelRequestStarted` y los terminales retienen la cerca de ledger
hasta publicar el evento. Runner/Resume consumen solicitudes en fronteras seguras
y reintentan sólo ese append si una solicitud ganó la carrera; no repiten la
inferencia ni un efecto. ResumeWorker bloquea efectos inciertos antes de preparar
MCP. La retirada de una solicitud Requested usa CAS y los mismos locks, sin
necesitar un segundo lease; si Start ganó, no afirma cancelación. Las respuestas
asíncronas de la UI conservan la revisión más reciente y no regresan un recibo
Cancelled/Finished a Requested.

La evidencia y los límites concretos están en
[native-command-operations.md](native-command-operations.md). La evidencia nueva
es Linux con procesos y HTTP locales. El resize visible depende del snapshot
ConPTY real; sus pruebas están condicionadas a Windows y no se compilaron ni
ejecutaron en esta corrida Linux. Durante un tool/hook/permiso en curso, la acción manual espera la siguiente
frontera segura. No hay restauración de procesos, nuevo historial incremental de
stdout ni aceptación de modelos externos por estas pruebas. Las capacidades del
roadmap siguen parciales y el goal permanece activo.


Verificación integrada posterior de este bloque: **1495 pruebas pasadas, 25
ignoradas, en 15 suites** con `cargo test --workspace` (22.06 s, concurrencia
predeterminada). Incluye 14 tests core del ledger, seis casos reales del actor,
cuatro casos HTTP/Resume, tres regresiones de política entre hooks, cuatro gates
desktop previos a MCP/Idle y quince tests UI/host del control. Los helpers de
cortes se ejecutan desde sus padres; la aceptación externa opt-in no se cuenta
como inferencia verificada. `cargo fmt --all` y su check, y `git diff --check`,
pasaron. La inspección visual nueva de Contexto se exportó desde HTTP real del
fixture a `/tmp/openide-native-context-final`: resumen 320×200 px con dos filas,
barra fina y estado sin medición tras checkpoint, sin inflar categorías a tokens.
El build/test del workspace se verifica junto a los cambios visuales compartidos;
no se atribuye todo su tamaño al harness.


`cargo clippy --workspace --all-targets` final terminó con **0 errores y 67
advertencias** del desktop (63 collapsible_match, dos variantes de distinto
tamaño, un collapsible_if y un unwrap tras is_some). Se eliminó el helper de
prueba sin llamadas que introducía una advertencia durante esta integración.
El gate estricto de `openide-core`, `openide-harness` y `openide-ui`, todos con
`--all-targets -- -D warnings`, pasó. El workspace aún no tiene un gate global
con -D warnings verde; estos resultados no equivalen a cerrar paridad funcional.

## Aislamiento Linux y vida de comandos entre turnos (2026-09-30)

El contraste con el código encontró un fallo de vida real: un manager retenido
conservaba su ID y buffer, pero sus Child/readers dependían del runtime del turno
inicial. Tras cerrar ese runtime, una escritura desde otro turno fallaba. El
reactor propio del manager corrige ese comportamiento sin restaurar procesos ni
mantener vivo el runtime del modelo. Cinco pruebas de
`persistent_runtime_lifetime` verifican actividad real en dos runtimes separados,
input Unicode/EOF, terminal, revoke, cierre de descendientes y Drop desde async.
Un descendiente Host escapado mantiene la tubería abierta para comprobar que la
gracia del reader no bloquea el caller; el fixture se elimina explícitamente.

El adaptador Linux propio comparte preparación con los comandos persistentes:
Bubblewrap para namespaces/montajes, filtro BPF propio, handshake antes del
payload, entorno mínimo, descriptores fijados y supervisión pidfd. La suite del
backend tiene seis pruebas, cinco OS opt-in ejecutadas: argv literal, archivos
externos/symlinks/runtime, sockets e io_uring, FD heredable cerrado, preparación
fallida sin payload, cancel/timeout/drop/salida con setsid y ABI x32/compat
denegadas. Se requiere runtime del sistema o ejecutable dentro del workspace;
home/rustup no se montan implícitamente. Se rechazan hardlinks y montajes anidados
en la inspección acotada, con límites y races del host documentados en
[linux-command-sandbox.md](linux-command-sandbox.md).

`persistent_linux_isolation` aporta otras cinco aceptaciones OS reales:
confinamiento y stdin binario tras cerrar el primer runtime; revoke, terminate,
timeout y drop; hardlink rechazado antes del payload; salida normal con hijo
setsid; y SIGKILL del dueño Rust sin destructores. En el último caso se detiene
el heartbeat externo del descendiente y un manager nuevo no acepta ni reejecuta
el ID previo. El comando usa el filtro `actual_linux` para no ejecutar el helper
de subprocess directamente:

```sh
rtk env PATH=/home/nihuel/.cargo/bin:/run/current-system/sw/bin:/home/nihuel/.local/bin nix shell nixpkgs#gcc nixpkgs#pkg-config nixpkgs#wayland nixpkgs#libxkbcommon nixpkgs#bubblewrap --command rtk cargo test -p openide-harness --test persistent_linux_isolation actual_linux -- --ignored
```

`native_linux_command_app_tests` verifica por separado el contrato desktop.
Una prueba default ejecuta un subprocess con PATH privado sin helper: preserva
el perfil guardado, bloquea la opción y aplica Deny antes del payload. Las dos
opt-in pasan con helper real: Settings a 360/900 con pointer y Tab/Enter,
guardar/reabrir, capacidades pipe sin resize, y el actor real con stdin/EOF,
lease ocupado, archivos/sockets denegados y recibos reabiertos sin manager.

Estas pruebas se ejecutaron en Linux 7.1.3 x86_64/Bubblewrap 0.11.2. Aarch64 no se
ejecutó, macOS sigue pendiente y Windows no se volvió a verificar en esta build.
No certifican modelos externos, publicación dentro de syscalls, otros workers,
historial incremental durable de salida ni aislamiento de hooks/MCP. Las filas
siguen parciales y el goal permanece activo.

Verificación integrada después de congelar este bloque: `cargo test --workspace`
pasó **1502 pruebas, 39 ignoradas, en 17 suites** (21.86 s, concurrencia
predeterminada). Se ejecutaron además las **12 aceptaciones OS opt-in** de Linux
descritas arriba: cinco del backend, cinco de comandos persistentes y dos del
desktop. Los helpers de subprocess no se invocaron como casos independientes.
El caso de argv literal incluye un ejecutable `./-name`; `-a`, `-c` y `--` se
rechazan antes de preparar el helper o inspeccionar el workspace, porque `exec`
de POSIX puede interpretarlos como opciones. El wrapper también se probó con
Dash 0.5.13.3 para portabilidad de argv y handshake, sin atribuirle una nueva
aceptación de aislamiento.

`cargo fmt --all` y su check, y `git diff --check`, pasaron.
`cargo clippy --workspace --all-targets` terminó con **0 errores y 67
advertencias** del desktop; el gate estricto de `openide-core`,
`openide-harness` y `openide-ui` con `--all-targets -- -D warnings` pasó.
Las advertencias globales y las capacidades pendientes siguen abiertas; estos
gates no equivalen a paridad funcional ni completan el goal.

## Validación inicial antes de MCP y controles (2026-09-30)

El contraste encontró dos fronteras: el runner comprobaba el límite del snapshot
de instrucciones y el plan después de preparar MCP, y su cierre de error podía
procesar la cola humana. Ahora distingue autoridad aún no verificada, admisión
actual exacta verificada y efectos habilitados. Comprueba las entradas iniciales
conocidas antes de habilitar preparación o controles; conserva el orden de los
commits de catálogos/instrucciones/contexto en turnos válidos.

`initial_instruction_preflight` tiene tres aceptaciones con procesos y HTTP
locales reales. Instrucciones sobre 512 KiB o plan inválido rechazan Requested
antes de MCP, hooks, stdin y proveedor; el límite exacto de instrucciones permite
todos esos efectos. Un retry de una admisión anterior no consume intenciones
posteriores. Stores con el mismo ID pero autoridad distinta y recibos Started
conservan sus bytes/estados, sin efecto ni cierre inventado. Los stores se vuelven
a abrir para verificar los recibos y journals. Las dos pruebas privadas
`initial_validation_boundary_tests` verifican la carrera de enqueue/Started en
la publicación y el mismatch de request/sink/lease. La carrera usa un wrapper de
prueba sobre el store real, no una muerte OS ni un nuevo criterio de cortes.

`native_initial_instructions_app_tests` agrega tres pruebas con cuatro setups
reales: AGENTS inválido UTF-8, sobrelímite, symlink Unix y skill seleccionado cuyo
contenido cambia. El rechazo desde `AgentsWindow::start_run` conserva borrador,
selección y store reabierto sin sesiones/admisiones, workers ni requests MCP o
del proveedor. Reparar el mismo archivo/configuración ejecuta initialize, dos
páginas tools/list, una petición nativa SSE con instrucciones y catálogo MCP,
TurnCompleted y DELETE. El fixture usa GET 405 para notificaciones opcionales;
no acepta notificaciones MCP ni identidad MCP para Resume.

El runner recibe texto ensamblado por el host, sin clasificar semánticamente su
confianza. Los catálogos e identidades dinámicas todavía requieren descubrir
MCP antes de validarlos. Estas pruebas no aceptan una cuenta externa, scopes
arbitrarios de comandos/herramientas externas, otras plataformas ni nuevos
cortes dentro de syscalls. La captura durable de comandos se implementó y
verificó en el bloque siguiente; los otros workers y plataformas siguen
pendientes. Todas estas capacidades conservan el estado Parcial y el goal sigue activo.

Verificación integrada de la fuente final: `cargo test --workspace` pasó
**1510 pruebas, 39 ignoradas, en 18 suites** (23.13 s, concurrencia
predeterminada). Las ocho pruebas nuevas son cinco del harness y tres del
desktop; el total también incluye trabajo visual compartido. La suite completa
del harness pasó por separado 481 pruebas, 26 ignoradas, en 11 suites. Las
regresiones existentes del fixture MCP pasaron 11/11 después de añadir su
constructor test-only de GET 405, preservando el constructor original.
`cargo fmt --all` y su check, y `git diff --check`, pasaron. Clippy del workspace
terminó con **0 errores y 67 advertencias** del desktop; core, harness y UI
pasaron el gate `--all-targets -- -D warnings`. Las aceptaciones Linux opt-in
registradas en el bloque anterior son independientes de estas ocho nuevas
pruebas; no se suman como otra aceptación de proveedor o de plataforma.

## Captura durable de salida y reapertura (2026-09-30)

El contraste con `persistent-commands.md` confirmó que el poll final consumía
bytes y retiraba el ID antes de publicar `ToolFinished`. Se implementó un
contrato propio de productor en core, independiente del buffer del agente y
del journal de herramientas. El desktop requiere adjuntarlo al manager antes
de su primer start; un store faltante no degrada esa ruta a captura en RAM.
El descriptor sincronizado tiene dueño nativo, workspace canónico, generación
e ID de comando exactos. Su existencia no confirma lanzamiento ni autoriza
otra ejecución.

El productor sincroniza cada ACK antes de exponer bytes u omisiones, serializa
stdout/stderr por cursor y confirma el terminal antes de notificarlo cuando la
publicación tiene éxito. El historial sobrevive a poll/remove y a reabrir los
stores sin manager. Conserva como máximo 8 MiB raw/4096 records/64 descriptores
por sesión, bajo un sobre serializado de 16 MiB con reserva para terminales.
Los límites agotados producen un rango omitido exacto, no una salida completa
ficticia. Los pages tienen límites de raw/records/serialización; la UI conserva
una página de hasta 16 KiB/32 records. No se afirma un límite de RSS o latencia.
Un fallo de append detiene el árbol; un terminal no confirmado no se inventa
tras reopen. Archive/delete/tree serializan con el store y un writer tardío no
recrea datos después del borrado.

Evidencia de comportamiento y recuperación:

- Doce pruebas core, incluida
  `durable_command_output_real_process_cuts_preserve_register_append_omission_and_terminal`:
  doce SIGKILL de register/append/omission/finish, cada uno en temp sincronizado,
  rename y directorio sincronizado. Reopen devuelve un artefacto completo
  anterior/nuevo y retry no duplica. Otras pruebas cubren dos handles, archive,
  delete/tree, corrupción semántica con checksum recalculado, symlinks, archivos
  privados y temporales ajenos/huérfanos. Las cuotas raw/records/serialización
  usan snapshots válidos cerca de capacidad y ejercitan append/read/reopen
  públicos; no son una aceptación de proceso emitiendo 8 MiB.
- Siete pruebas de proceso de `persistent_native_output`, dos privadas del
  productor y una opt-in Linux offline ejecutada explícitamente. Cubren
  binario/Unicode, runtimes separados, ACK bloqueado antes de observación,
  omisión real por quota, fallo antes del payload y fallo de captura que detiene
  el proceso. Un caso de fallo de publicación conserva el artefacto confirmado
  byte por byte y lo reabre sin reparación ni retry. Dos casos de corrupción
  restauran explícitamente la copia válida del fixture sólo para inspeccionarla;
  no se anuncia reparación automática del producto. El adaptador Windows se
  ajustó a esta publicación/contención, sin nueva ejecución en Windows.
- `durable_output_survives_final_poll_publication_cuts_without_reexecution_or_synthesized_tool_result`
  ejecuta Runner/HTTP/SSE, permiso y shell reales. Mata con SIGKILL antes y
  después del commit del `ToolFinished` de poll final, con el ID ya retirado.
  Reabre stores, lee páginas de tres bytes y conserva exactamente stdout/stderr
  binarios y terminal. El corte anterior mantiene ToolOutcomeUncertain sin
  síntesis ni requests; el posterior hace Resume y la siguiente petición HTTP.
  El efecto ocurre una vez en ambos casos. Un manager nuevo no restaura el ID.
- Actividad → **Historial de salida** muestra salida legible con stream/rango,
  omisiones e incompletitud, base64 para segmentos binarios o UTF-8 dividido y
  datos exactos JSON opcionales. Detalles conserva argv, identidad y errores
  completos en scroll. Las aceptaciones App usan procesos, navegación pointer/
  teclado, final poll y reopen sin manager, junto con rechazos de respuestas
  obsoletas por store/scope/binding/request/cursor/revisión. Exports nativos
  Frame/SVG/resvg a 360/900 px fueron generados e inspeccionados. El host pide
  repaint durante consultas pendientes sin manager ni turno activo. El worker
  lee metadata/output; no llama a `SessionStore::load`, que podría reparar un
  tail del journal durante una inspección que debe ser de sólo lectura. Dos
  aceptaciones nuevas de las cinco App verifican esos últimos ajustes:
  `durable_output_reopened_history_requests_repaint_and_applies_response_through_real_egui_frame`
  ejecuta App::ui con barrera de respuesta, baseline idle y page real aplicada
  sin poll manual;
  `durable_output_history_reads_capture_without_repairing_torn_model_journal`
  abre el worker de una captura real y conserva el JSONL truncado byte por byte.

Ver el contrato y pruebas exactas en
[native-command-output.md](native-command-output.md). Persistencia de salida
no restaura stdin/procesos, no reconstruye `ToolFinished` faltante y no retira
el gate humano de incertidumbre. Este bloque precede a la conexión de hijos
al registro compartido, ver Comandos nativos por hijo abajo. Las automatizaciones
siguen sin manager/captura. Faltan Windows ConPTY/MXC, ARM64/macOS, cuentas reales, cortes
dentro de syscalls y power loss. La presencia de credenciales se verificó sólo
en variables de entorno conocidas, todas ausentes para esta aceptación; no se
certifica un inventario completo del vault ni inferencia real. Ninguna fila
se declara completa y el goal sigue activo.

Gate integrado de la fuente final: `cargo test --workspace` pasó **1537 pruebas,
43 ignoradas, en 20 suites (39.52 s)** con concurrencia predeterminada. Frente
al gate anterior de 1510, este bloque añade 27 aceptaciones default: doce core,
diez harness y cinco App. Las cuatro nuevas ignoradas son dos helpers de cortes
invocados por sus padres, la aceptación Linux offline ejecutada explícitamente
y el export visual ejecutado aparte; no se cuentan como cuentas externas.
El primer intento global se interrumpió por falta de espacio durante el enlace;
se eliminó sólo `target/debug/incremental`, caché regenerable del proyecto, y
se repitió el gate hasta completarlo. `cargo fmt --all`, su check y
`git diff --check` pasaron. El gate estricto de core/harness/UI
`cargo clippy -p openide-core -p openide-harness -p openide-ui --all-targets -- -D warnings`
pasó. `cargo clippy --workspace --all-targets` terminó con **0 errores y 67
advertencias** del desktop: 63 `collapsible_match`, dos variantes grandes,
un `collapsible_if` y un `unwrap` después de comprobar `is_some` en Git.
Ni estos resultados ni el historial durable equivalen a paridad funcional.

## Comandos nativos por hijo y revocación del subárbol (2026-09-30)

El contraste del roadmap con el host confirmó que los hijos construían sus
herramientas sin manager/captura, aunque los permisos permitían `run_command`.
Ahora `NativeCommandRegistry` conserva managers por dueño nativo y entrega
perfiles a ejecución normal, parent Resume, child recovery y mailbox dispatch.
Los perfiles son capacidades del host con referencia débil y epoch; no crean
procesos al consultar schemas. Manager y perfil comparten el mismo validador
de límites y builder de catálogo. Cada manager adjunta captura durable antes
del primer spawn; falta de store, scope extraño y corrupción bloquean payload.

El worker instala el overlay bajo el lease propio del hijo y una base que ya
anuncia `run_command`. Cada hijo/nivel obtiene su manager; no hereda IDs del
padre o hermanos. El actor del registro es idéntico al usado por Runner/Resume
y controles humanos, por lo que notifica stdin/EOF durante la espera del modelo
bajo el lease ocupado. Límites/permisos/hooks del modelo siguen congelados;
los controles humanos recargan política y hooks. Un Completed conserva su
manager para follow-ups entre runtimes; Failed/Cancelled y revocación del host
lo cierran. Reopen conserva captura, sin restaurar el proceso ni su ID.

La identidad durable vincula el perfil configurado sin epoch/generación.
Recovery selecciona el candidato sin perfil para reservas legacy: no amplía
su catálogo. Plan no anuncia ni ejecuta comandos. La revisión posterior al
primer gate encontró una frontera adicional: si la primera reserva legacy
era Plan, la huella no distinguía perfiles y su follow-up Build podía añadir
comandos. La corrección vincula el perfil optativo en todos los modos,
conservando exactamente las identidades anteriores sin perfil; la aceptación
de la transición Plan→Build compara reservas legacy y nuevas.

Plan conecta `actor_existing` si el host ya conserva un manager: valida store,
lease, workspace, opciones y epoch bajo el mismo lock antes de devolver el
actor compartido. No crea manager/captura/runtime ni amplía herramientas del
modelo. El mailbox admite sólo follow-up Build; la aceptación Plan de controles
humanos usa un comando real creado por el host antes del worker inicial, no
una transición Build→Plan del mailbox. Runner y Resume tienen este binding.

La cancelación del coordinador devuelve los IDs exactos del subárbol sólo tras
el commit y bajo el mismo lock que creación/claim. Incluye hijos/descendientes
Completed, Failed e idle para cerrar sus managers. Bridge, herramienta y UI
sin bridge revocan esos dueños antes de notificar tokens. Un rechazo no publica
reconciliación ni cambios de cola. La bandera durable impide nuevas reservas
descendientes, manteniendo lookup de retries históricos. La limpieza autorizada
por claim o recibo de cancelación cierra también managers emitidos con un
perfil más nuevo para ese dueño; un perfil de creación viejo sigue bloqueado.

Evidencia local, además de las fronteras de productor/actor ya documentadas:

- Nueve pruebas registry default y
  `offline_registry_uses_real_confinement_and_durable_capture_without_host_fallback`,
  opt-in ejecutada con Bubblewrap. Verifican schemas puros/exactos, scope/store,
  límites, independencia de dueños y runtimes, actor compartido, epoch, Drop
  con referencias retenidas y revocación incluso con mutex envenenado.
  `existing_actor_lookup_is_pure_owner_bound_and_checks_store_workspace_profile_and_epoch`
  verifica además lookup vacío sin publicación, handles/leases ajenos,
  workspace/perfil incluso con actor cacheado y epochs revocados.
- Worker real con backend local y procesos: hooks pre/post reciben sesión/tool
  exactos; Unicode/EOF y final poll cruzan runtimes; IDs padre/hermano fallan;
  legacy Ready/Resume y Plan conservan el catálogo; permisos Deny y revocación
  antes de Allow no registran captura ni ejecutan payload. Otro caso sostiene
  un proceso durante model wait y verifica commit→revocación→notificación del
  bridge. Seis pruebas del worker incluyen la transición legacy/nueva Plan→Build
  y el Plan con comandos creados por el host, sin admitir un follow-up Plan.
  Los nombres exactos y límites están en
  [native-subagents.md](native-subagents.md).
- `cancellation_returns_complete_committed_subtree_after_reopen`,
  `cancellation_authority_failures_do_not_reconcile_or_publish_targets` y
  `cancellation_closes_new_descendant_spawn_but_preserves_existing_retry`
  comprueban cleanup targets completos/reopen, rechazo sin publicación y
  creación/retry bajo la bandera de cancelación. El módulo coordinador pasa
  29 pruebas.
- `child_commands_share_host_controls_across_model_wait_followup_cancel_and_reopen`
  ejecuta App/HTTP/SSE y proceso reales. La UI aprueba `start_command` del hijo,
  entrega texto Unicode con CRLF/CR/TAB/NUL y EOF mediante pointer a 360/900 px
  mientras espera la respuesta del modelo. Un worker reconstruido por el host
  procesa el follow-up real de `send_input`, consume poll final y conserva
  bytes/terminal. Cancelación UI de Completed sin bridge cierra un proceso
  adicional iniciado directamente como fixture. Reabrir App muestra el mismo
  historial sin manager. No se atribuye ese fixture extra a otra tool del modelo.

No se afirma aceptación de cuenta externa, ejecución nueva de Windows
ConPTY/MXC, macOS/ARM64, todos los cortes del hijo ni power loss. El historial
no autoriza replay de un efecto incierto. Comandos persistentes, subagentes y
UI permanecen Parcial por estos criterios y los de automatizaciones.

### Auditoría previa de automatizaciones

Antes de la integración de origen durable descrita al final de este documento,
`dispatch_automation` ofrecía lecturas locales y denegaba aprobaciones en
background. El vínculo del ledger se guardaba después de arrancar el worker;
no había snapshot de autoridad en la sesión y Resume anterior al marcador de
contexto podía reconstruir el perfil desde Settings. Esa auditoría motivó el
nuevo contrato [native-automations.md](native-automations.md).

El nuevo origen resuelve esa diferencia para el perfil original de lectura.
En ese primer tramo seguían pendientes efectos, permisos por automatización,
aprobaciones con alcance a run/sesión/request y resultado terminal durable.
El resultado se implementa en la aceptación posterior. No se habilitan comandos
por heredar permisos globales.

### Gate integrado tras las correcciones de revisión

`cargo test --workspace` pasó **1556 pruebas, 44 ignoradas, 20 suites
(39.49 s)** con concurrencia predeterminada. Frente al bloque de captura durable
anterior (1537), se añaden 19 pruebas default: nueve registry, seis worker,
tres coordinador y una App. La nueva ignorada es la aceptación Bubblewrap del
registro, ejecutada explícitamente aparte. Los dos hallazgos de revisión se
corrigieron antes de repetir este gate: identidad de Plan legacy y binding
del actor existente en Plan. Los helpers de cortes previos y otras aceptaciones
opt-in conservan sus filtros; 44 ignoradas no significa 44 cuentas verificadas.

`cargo fmt --all`, su check y `git diff --check` pasaron.
`cargo clippy --workspace --all-targets` terminó con **0 errores y las mismas
67 advertencias** del desktop: 63 `collapsible_match`, dos variantes grandes,
un `collapsible_if` y un `unwrap` después de `is_some` en Git. La aceptación
de transporte/captura es local, no paridad funcional completa.
El gate estricto
`cargo clippy -p openide-core -p openide-harness -p openide-ui --all-targets -- -D warnings`
pasó sin diagnósticos sobre la fuente final.

En esta etapa se comprobó sólo presencia de credenciales, sin imprimir sus
valores: las siete referencias `key_env` de las definiciones nativas incluidas
estaban ausentes (`DEEPSEEK_API_KEY`, `GROQ_API_KEY`, `MISTRAL_API_KEY`,
`OPENAI_API_KEY`, `OPENIDE_API_KEY`, `OPENROUTER_API_KEY`, `XAI_API_KEY`).
No había `provider-registry.json` en el data root resuelto del proceso.
También estaban ausentes las variables Anthropic/Gemini/Google revisadas; eso
no constituye soporte de esos protocolos ni aceptación de generación.
No se hizo inferencia de cuenta externa ni inventario de un vault remoto.
La aceptación con cuentas/modelos reales sigue pendiente. El origen durable y
la recuperación del perfil original de lectura se verifican en el tramo siguiente.

## Automatizaciones: autoridad original y recuperación (Linux, 2026-09-30)

El contrato de [native-automations.md](native-automations.md) conserva el
workspace canónico, proveedor/ID/protocolo, capacidades exactas, límites de
salida/contexto, presupuesto, instrucciones y prompt originales en metadata
atómica. Guarda sólo el nombre de la variable de credencial, sin su secreto.
El registro precede al enlace del ledger, admisión y arranque del worker. Un
retry encuentra la misma sesión y no vuelve a ejecutar un journal existente.
ReadOnlyV1 anuncia `read_file`, `list_files`, `search_files` y `update_plan`; no instala
MCP, hooks, goals, hijos o comandos desde la configuración interactiva actual.

La aceptación nueva comprende:

- **Core:** ocho pruebas default de origen y un helper ignorado ejecutado por
  su padre en tres cortes de publicación de metadata, más una prueba de rechazo
  de intentos manuales/captura después de reabrir. La metadata preserva origen
  ante cambios de título, pin y archivo; IDs conflictivos, corrupción y
  tombstones no se reparan ni recrean implícitamente.
- **DTO/transporte:** cinco pruebas preservan Chat y Responses, campos exactos,
  capacidades, output cap, presupuesto y búsqueda alojada con ajustes dañados;
  un snapshot inválido falla antes de leer credenciales. HTTP local y clave de
  fixture; no son cuentas externas.
- **ResumeWorker:** dos pruebas default y un helper ignorado. Diez subprocesos
  salen sin destructores después de admisión, catálogo, política, instrucciones,
  contexto, ruta, pedido/respuesta de modelo e inicio/resultado de lectura.
  Cada reapertura termina con una admisión, dos requests SSE y una llamada
  `read_file`, sin alterar origen o prefijo guardado. ToolStarted sin resultado
  exige verificación explícita de la lectura. Catorce mezclas de autoridad se
  rechazan antes de preparación, transporte o append.
- **App:** dos pruebas ejecutan dispatch/retry y recuperación mediante la
  acción UI de una metadata sin admisión. Instalan una política interactiva
  más amplia, hooks y MCP canary sin producir sus efectos. Corrompen Settings,
  registro de proveedores, límites, AGENTS y extensiones; la ruta y las
  instrucciones originales ejecutan lectura real. La segunda compacta por
  HTTP sin herramientas y reabre un checkpoint con checksum/compatibilidad.
- **Scope y superficie:** una prueba Unix sustituye el workspace registrado
  por un symlink a otro proyecto; App y worker lo rechazan sin request o append
  y mantienen el historial consultable. Las acciones retrasadas de goals no
  mutan el sidecar legacy. La tarjeta sólo lectura conserva Contexto con ruta
  original y owner/workspace exactos frente a Settings cambiados: resumen de
  320 × 200 px, detalles separados, pointer y cierre a 360/900 px; un uso ajeno
  permanece Sin medición.
- **Ledger/controles:** cinco pruebas nuevas distinguen enlace de despacho,
  conservan sesión ante fallo, verifican reopen/retry y claims recortados. Una
  prueba de vista distingue registro pendiente de despacho. Cuatro pruebas
  nuevas previas cubren origen/recuperación, impedir envío/steering, detener y
  nueva tarea, y edición de memoria bloqueada. El registry rechaza manager,
  actor y overlay de comandos para ese origen.

La aceptación encontró y corrigió dos diferencias del motor: el workspace
podía retargetearse antes del primer contexto; la jerarquía dinámica volvía a
leer AGENTS actuales aunque las instrucciones originales estaban congeladas.
La ejecución vuelve a comprobar la ruta física antes de credenciales/admisión
o recovery; ReadOnlyV1 usa únicamente las instrucciones registradas. Las
sesiones interactivas conservan la lectura jerárquica por directorio. El check
de ruta no mantiene un inode/descriptor ni cierra reemplazos posteriores a la
validación; estos cortes no simulan power loss ni interrupciones dentro de
una syscall o un proceso GUI completo.

Continúan pendientes efectos, permisos/aprobaciones por automatización,
resultado terminal durable en el ledger y scheduler entre múltiples procesos
desktop. Ninguna fila se declara completa por estas aceptaciones locales.

El resultado terminal del ledger, pendiente en esa aceptación de origen, se
implementa y verifica en el bloque posterior de proyección de resultados.

### Gate integrado del origen ReadOnlyV1

`cargo test --workspace` pasó **1587 pruebas, 46 ignoradas, 20 suites
(39.20 s)** con concurrencia predeterminada: 31 pruebas default más que el
gate previo y dos helpers de corte adicionales ignorados que sus tests padre
ejecutan explícitamente. La primera corrida encontró una regresión del texto
de diagnóstico de lectura I/O en el nuevo guard; se conservaban borrador y
bloqueo de ejecución. El guard ahora mantiene «No se pudo leer la sesión
nativa» con la causa, y la corrida completa posterior pasó.

`cargo fmt --all`, su check y `git diff --check` pasaron.

`cargo clippy --workspace --all-targets` pasó con **0 errores y las mismas
67 advertencias** existentes del desktop: 63 `collapsible_match`, dos variantes
grandes en Git/MCP, un `collapsible_if` y un `unwrap` tras `is_some` en Git.
El gate estricto
`cargo clippy -p openide-core -p openide-harness -p openide-ui --all-targets -- -D warnings`
pasó sin diagnósticos.

Se volvió a comprobar sólo presencia de las siete referencias de credencial
nativas: `DEEPSEEK_API_KEY`, `GROQ_API_KEY`, `MISTRAL_API_KEY`,
`OPENAI_API_KEY`, `OPENIDE_API_KEY`, `OPENROUTER_API_KEY` y `XAI_API_KEY`
estaban ausentes. No había `provider-registry.json` en
`/home/nihuel/.local/share/openide`, el data root resuelto del proceso. No se
consultó un vault remoto ni se hizo inferencia con una cuenta externa. El goal
permanece activo; los criterios pendientes de la tabla siguen abiertos.

## Automatizaciones: resultado durable y diagnóstico por proyecto

`AutomationRun.status` sigue describiendo despacho. `native_result` V1 es una
proyección del journal con Completed/Failed/Cancelled/Interrupted,
recuperación requerida, origen/ocurrencia/workspace, primer turno, digest de
autoridad, terminal y prefijo observado. El host toma un lease libre y vuelve
a cargar de forma estricta antes de derivar, con replay/checkpoints y perfil
original, sin consultar Settings, claves o modelos. Un worker ocupado se
difiere; metadata sin admisión conserva resultado ausente. Un token privado
sin Deserialize prueba la derivación semántica nueva al ledger; el DTO
guardado por sí solo no se acepta como prueba.

La observación rechaza prefijos modificados, secuencias antiguas, scopes ajenos,
terminales reemplazados y claims recortados. Failed/Cancelled pueden resolver
su lote pendiente sin convertirse en Completed. La reconciliación se ejecuta
en startup, cierre de worker/Resume, compactación, verificación/descarte de
llamadas, retry de despacho y tick de 30 segundos. Si el cierre del canal llega
antes de liberar el lease, la consulta posterior resuelve la proyección.
Guardar sincroniza archivo y directorio padre después del rename en Unix;
el journal y ledger no son una transacción conjunta. El ledger continúa
limitado a 256 entradas/2 MiB y a un escritor desktop por archivo.

La evidencia nueva es:

- **Seis App:** HTTP/SSE real→Completed→ledger/reopen con Settings/registry
  dañados; lease ocupado después de delta sin falso Interrupted; dos cortes
  reales sin destructores, uno después del delta y otro después de
  TurnCompleted antes del ACK del ledger. Reapertura deriva Interrupted y
  recupera por ResumeWorker, o deriva Completed sin otra request/admisión.
  El helper ignorado se ejecuta desde los dos tests padre. Fallo de transporte
  y cancelación reales después de delta conservan Failed/Cancelled al reabrir.
  Un checkpoint semántico por HTTP y los casos missing/corrupt de journal o
  checkpoint conservan último resultado y aviso actual sin reparar bytes.
  La regresión de dos workspaces comparte definición/run ID, conserva sesiones
  y pruebas distintas y no pierde el diagnóstico de A al observar B; B usa un
  prefijo fixture de fallo antes de modelo, no otra aceptación HTTP.
- **Cinco ledger y dos UI:** enlace/resultado distintos de despacho,
  reopen/idempotencia, extensión exacta, rechazo de stale/prefix/terminal/scope,
  fallo de publicación, recorte sin recrear claim, UI con último resultado,
  diagnóstico de despacho y aviso actual completos, pointer a 360/900 px.
  La quinta ledger conserva un proveedor arbitrario en create/update/reopen
  y rechaza selección malformada sin leer credenciales. El borrador antes
  llamaba a `resolve()` contra builtins; ahora valida sólo forma y el despacho
  conserva su validación del registro real antes de admitir.
- **Dos derivador:** prefijos de journal fixture de lectura incierta con
  Failed/Cancelled se verifican mediante la API real de recuperación, append
  de un resultado y reopen; conservan terminal, cierran recuperación y nunca
  fabrican Completed. Ruta/contexto mezclados y segundo terminal se rechazan
  sin cambiar el journal original. No se anuncia inferencia o efecto real
  en esos prefijos fixture.
- **Una core y una harness:** `load_strict` conserva bytes de cola truncada o
  sin newline; la recuperación normal conserva su contrato de reparación
  explícita. El digest histórico de contexto coincide con el de ejecución
  antes de retirar el directorio y continúa calculable después de retirarlo;
  el gate físico de ejecución sigue rechazando esa ruta.

La revisión encontró dos fallos antes del gate final: hashes de un DTO
deserializado no probaban replay/perfil semántico, y diagnósticos indexados sólo
por run ID se pisaban entre proyectos. Se corrigieron con el token de
derivación y la clave workspace/run, respectivamente. Un diagnóstico actual
inválido conserva el último resultado guardado, sin presentarlo como una
verificación nueva. Estos checksums no autentican alteraciones hechas por
un escritor de los stores.

La consulta es síncrona sobre entradas retenidas y no certifica latencia con
journals grandes, power loss, corte dentro de syscall ni Windows/macOS. Siguen
pendientes efectos/permisos/aprobaciones por automatización, scheduler con
múltiples escritores desktop y aceptación con cuentas/modelos reales; la
capacidad de automatizaciones continúa Parcial.

### Gate integrado de resultados nativos

`cargo test --workspace` pasó **1604 pruebas, 47 ignoradas, 20 suites
(39.49 s)**, con concurrencia predeterminada. Frente al gate de origen
(1587/46), agrega 17 pruebas default: cinco ledger, dos UI, seis App, dos del
derivador, una core y una harness. La nueva ignorada es el helper de proceso
ejecutado dos veces por sus tests padre. Los 47 casos ignorados no se anuncian
como inferencia con proveedores verificada.

`cargo fmt --all`, su check y `git diff --check` pasaron.
`cargo clippy --workspace --all-targets` terminó con **0 errores y las mismas
67 advertencias** del desktop, sin diagnósticos nuevos: 63 `collapsible_match`,
dos variantes grandes en Git/MCP, un `collapsible_if` y un `unwrap` tras
`is_some` en Git.
El gate estricto
`cargo clippy -p openide-core -p openide-harness -p openide-ui --all-targets -- -D warnings`
pasó sin diagnósticos.

No se volvió a consultar un proveedor externo en este tramo;
el inventario de las siete referencias de credencial arriba sigue ausente.
La aceptación con cuentas/modelos reales permanece pendiente y el goal activo.

## Messages propio y catálogo paginado (2026-09-30)

El tercer protocolo nativo y su integración de catálogo, presupuesto, autoridad
ReadOnlyV1 y UI se documentan en [native-anthropic.md](native-anthropic.md).
La aceptación local incluye cortes reales, stores/checkpoints reabiertos, firmas
y resultados exactos. ANTHROPIC_API_KEY no está disponible: no se verificó una
cuenta externa. Estas evidencias amplían las filas parciales; no cierran aliases,
otros bloques, salida multimodal, thinking configurable desde UI, más plataformas
ni los demás criterios pendientes de esta tabla.


## Gate de Messages y presentación de conexiones (2026-09-30)

El usuario confirmó la selección visual de Codex, OpenRouter, OpenCode y NVIDIA.
El registro/discovery del motor permanece abierto. La regresión de catálogo
registra un ID arbitrario, recorre dos páginas HTTP y reabre mediante el loader
público; NativeProvider pasó 31 pruebas y ModelCatalog 8, con una externa
ignorada. Dos regresiones de metadatos verifican refresh/reopen y autoridad de
límites/capacidades desconocidos. Tres aceptaciones App Messages comprueban la
fábrica, ReadOnlyV1 y contrato de componentes con runner/journal reales locales;
no acreditan una cuenta Anthropic en el selector de cuatro conexiones.

Se corrigió una regresión del filtro visual que reemplazaba la identidad del
motor por campos vacíos. `native_composer_without_explicit_model_rejects_before_preparing_or_admitting`,
`hidden_registered_connection_preserves_engine_route_without_exposing_a_fallback_model`,
`native_context_http_reopens_checkpoint_refills_and_rechecks_exact_route_without_effects`
y `startup_dispatch_reopens_http_child_and_drains_frozen_mailbox_once` pasan en
la suite de la app: ausencia de fallback visible, navegación sin admisión,
recuperación de stores/hijos y ruta guardada conservada. Los fixtures visuales
de las cuatro conexiones usan cuentas sintéticas privadas; no envían inferencia
a NVIDIA/OpenCode/OpenRouter. El backend Messages sigue verificándose con un
proveedor arbitrario registrado, separado de esa decisión visual.

`cargo test --workspace` pasó **1653 pruebas, 50 ignoradas, 23 suites (42.50 s)**,
con concurrencia predeterminada. Una prueba local posterior del diagnóstico de
aceptación Codex pasó por separado. `cargo fmt --all`, su check y `git diff --check`
pasaron. `cargo clippy --workspace --all-targets` terminó con **0 errores y las
67 advertencias previas** del desktop. El gate estricto de core/harness/UI con
`--all-targets -- -D warnings` pasó. Las corridas intermedias fallidas durante la
integración de la UI fueron corregidas; no se confunden estos gates con cierre
funcional de las filas parciales.

La inspección de datos reales de la app encontró una credencial OAuth privada
de Codex; las otras tres conexiones no tienen un record guardado en ese root.
La aceptación optativa `saved_codex_plan_uses_exact_production_factory_and_reopens_durable_native_history`
se ejecutó dos veces contra el ID guardado `gpt-5.6-luna` y la ruta de producción
`https://api.openai.com/v1`. Cada ejecución admitió un único paso de modelo;
ninguna comprometió una respuesta. El último diagnóstico es `protocol_sse`,
sin status HTTP identificado, con un TurnFailed durable, journal reabierto y
archivos personales intactos. No se ejecutaron Build, herramientas,
compactación ni Resume externos. El límite es una llamada lógica por prueba;
el transporte conserva hasta tres intentos transitorios antes de texto observable.
Se retiene únicamente un JSON privado del diagnóstico, sin cuerpo ni credenciales.
El contrato de autenticación/endpoint, catálogo autenticado e inferencia de esa
cuenta siguen pendientes de verificación. ANTHROPIC_API_KEY continúa ausente.
El goal sigue activo mientras queden criterios de la tabla.
