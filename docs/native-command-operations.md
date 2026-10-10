# Operaciones humanas de comandos persistentes

## Contrato durable del core

`NativeCommandOperationStore` guarda intenciones y recibos de controles humanos
en `<session>.native-command-operations.json`. Este archivo es independiente del
journal del modelo: guardar una intención no genera `ToolStarted`, una respuesta
del modelo ni una aprobación ficticia. El core no escribe stdin, ejecuta hooks,
redimensiona terminales, termina procesos ni restaura procesos al reabrir.

Cada intención liga el ID de la sesión nativa y su workspace canónico con UUID
v4 completos de operación, generación del manager y comando. Conserva los
digests SHA-256 exactos de la política y de hooks aprobados. La acción es una de:

- `Stdin { input, close_after }`: UTF-8 exacto, incluyendo CR, LF y Unicode;
  no añade un salto de línea. Input vacío sólo es válido para EOF explícito.
- `Resize { columns, rows }`: dimensiones solicitadas, no una afirmación sobre
  las dimensiones actuales del proceso.
- `Terminate`: control del mismo comando y generación, sin permiso implícito.

`enqueue_requested` puede guardar sólo la intención mientras otro actor posee
`NativeSessionLease`. Su transacción usa el mismo orden que admisión y eliminación:
mutex del store, lock del journal y lock del ledger. Valida metadata nativa,
workspace e identidad; rechaza intenciones nuevas en sesiones archivadas o con
eliminación pendiente. Repetir ID y argumentos exactos devuelve el snapshot
guardado, incluso si ya terminó. Cambiar argumentos bajo ese ID genera conflicto.

Antes de efectos, el actor debe revalidar permisos, hooks y la generación real.
Los digests guardados describen la confirmación; por sí solos no autorizan nada.
Un manager nuevo no puede cumplir una intención de la generación anterior.
El core permite rechazar/cancelar `Requested` con una razón durable y sin efecto.
`cancel_queued_requested` permite retirar una intención mientras el worker tiene
el lease. Usa la misma transacción del journal/ledger y revisión exacta que Start:
si Start ganó la carrera, la retirada genera conflicto y nunca guarda Cancelled
para un efecto ya iniciado. Repetir la retirada con revisión y razón originales
devuelve el recibo; un ID desconocido no crea operación ni evento.

## Inicio, fases y recuperación

Las mutaciones requieren el lease nativo del mismo store y una revisión exacta:

1. `preflight_capacity` reserva espacio durable para los recibos futuros sin
   modificar la revisión de la operación.
2. `begin_started` publica y sincroniza `Started / BeforePreHooks` antes de
   cualquier hook o efecto. Sólo `newly_started = true` permite continuar al
   actor que recibió ese resultado. Un retry devuelve `false`: no permite
   volver a ejecutar hooks, stdin, EOF, resize ni terminate.
3. `record_phase` conserva diagnósticos completos y límites previos a cada
   efecto. Las fases avanzan `BeforePreHooks` → `BeforeCommand` →
   `BeforePostHooks`; pueden repetirse para identificar hooks individuales.
4. `record_effect_result` guarda el resultado conocido del comando antes de
   post-hooks. Sólo un resultado conocido puede entrar en `BeforePostHooks`.
5. `finish` conserva el resultado agregado, separado de `effect_result`. Un
   post-hook que falla no borra el stdin/EOF/resize/terminate ya observado.
   Éxito agregado requiere un recibo de efecto conocido exitoso.

Los diagnósticos de fase pueden incluir JSON con ID de hook, decisión, razón,
stdout, stderr y error exactos. `phase_diagnostics()` devuelve esas observaciones
con revisión y timestamp; no relee archivos ni reconoce efectos inciertos.
`effect_result` nunca implica que un hook anterior no produjo otros efectos.
La clasificación de fallo conocido frente a efecto incierto corresponde al
actor que realmente observó el proceso/hook.

`Started` sin finish y `Finished / EffectUncertain` exigen verificación manual.
`resolve_started` conserva una nota humana como `Resolved`; no inventa éxito,
fallo o rollback, ni vuelve a ejecutar nada. Un retry de una mutación ya publicada
debe coincidir con la revisión y argumentos originales y no publica otra revisión.

`snapshot`, `list` y `list_pending` son lecturas. Nunca drenan output del comando,
eliminan recibos ni recuperan procesos. `require_no_uncertain` permite que el
actor consuma intenciones `Requested`, pero bloquea efectos sin verificar.
`require_no_pending` también bloquea intenciones que todavía no fueron consumidas.

## Cercas transaccionales y eliminación

`SessionStore` retiene el lock del ledger hasta publicar una admisión nueva,
`ModelRequestStarted` o un evento terminal del turno. Si hay una intención
pendiente o un efecto sin verificar, la publicación devuelve
`NativeCommandOperationAdmissionBlocked`; el actor debe consumir/rechazar lo
pendiente o exigir verificación y reintentar en una frontera segura. Un retry
byte-equivalente de un evento ya publicado sigue devolviendo su resultado previo.

Archivar, eliminar una sesión o eliminar su árbol nativo también requiere un
ledger sin operaciones pendientes. La eliminación mantiene los locks y leases,
limpia sólo el ledger y temporales UUID v4 propios antes de eliminar metadata.
Un ledger corrupto u huérfano se conserva y bloquea la operación; no se borra
automáticamente como si fuera un recibo válido. Los recibos no viven en el
checkpoint y no desaparecen al compactar el journal del modelo.

## Límites y publicación

- Ledger máximo: 16 MiB; hasta 256 operaciones y 128 cambios por operación.
- Input máximo: 8 KiB de bytes UTF-8, no cantidad de caracteres.
- Resize válido: columnas 20–500 y filas 5–200; la disponibilidad real depende
  del manager/plataforma y debe comprobarla el actor.
- Diagnóstico de fase/resultado: máximo 512 KiB. Razones de rechazo/cancelación
  y notas de reconocimiento: máximo 16 KiB. No se truncan silenciosamente.
- Cada intención reserva 256 KiB para poder rechazarla/cancelarla. Antes de
  Start, el actor reserva además una cota conservadora del JSON futuro de hooks
  y resultados. Nuevas intenciones no pueden ocupar ese espacio. Los cambios
  consumen los bytes reservados; un efecto incierto conserva espacio para su
  reconocimiento manual. El agotamiento produce error, sin podar el historial.

La escritura publica una copia temporal completa, hace `sync_all`, la renombra
atómicamente y, en Unix, sincroniza el directorio. `begin_started` retorna permiso
de avanzar sólo después de esa publicación. Los archivos creados en Unix usan
modo 0600 porque guardan input exacto. `recover_temporaries` requiere lease,
lock del ledger y validación del archivo publicado; conserva archivos ajenos,
directorios y symlinks. La sincronización de directorio no se afirma en Windows.
Una publicación nueva de intención puede limpiar temporales propios bajo su
transacción del journal/ledger sin adquirir el lease del worker; no ejecuta
efectos ni modifica estados del proceso.

## Evidencia del core y límites de esta evidencia

`native_command_operation_tests.rs` usa `SessionStore` real, reapertura de stores
independientes, publicación atómica y procesos hijos cortados después de
sincronizar el temporal, renombrar y sincronizar el directorio. Cubre intención,
reserva, inicio, fase previa al comando, resultado conocido, incertidumbre,
reconocimiento, rechazo y cancelación. También verifica CAS concurrente (un solo
Start nuevo), carrera cola/terminal, fences de modelo/admisión, preservación de
input Unicode/CRLF, límites, corrupción, workspace/lease ajeno y protección de
archive/delete/tree. Los tests comprueban que el journal del modelo no recibe
eventos ficticios por las operaciones humanas.
También cubre retiro de la cola mientras el lease está ocupado y 24 carreras
Start/Cancel entre handles de store independientes, con un solo ganador y
preservación de Started cuando la intención ya pasó a efectos.

Estos tests prueban el ledger y sus cercas. La ejecución real de procesos/hooks,
el consumo durante una sesión activa y la presentación de controles humanos
requieren las pruebas de integración del actor y de la UI; no se deducen de la
compilación ni de los recibos sintéticos del core. Los cortes de proceso no son
una simulación de pérdida de energía del sistema ni de fallos de hardware.

## Evidencia del actor con procesos reales

`openide-harness/tests/manual_command_recovery.rs` verifica seis escenarios en
Unix con procesos e instrucciones reales del actor:

- `confirmed_stdin_eof_and_terminate_touch_real_processes_without_fake_journal_events`:
  echo exacto de UTF-8/CRLF/NUL, EOF separado, terminate y retries de los mismos
  recibos después de cerrar stdin/terminar, sin nueva intención ni efecto.
- `fresh_deny_changed_hook_bytes_cancellation_and_capacity_rejection_precede_all_effects`:
  política Deny posterior a la confirmación, bytes de un script de hook que
  cambian, cancelación previa y reserva demasiado grande. Todos quedan en
  rechazo/cancelación durable sin input ni hooks ejecutados.
- `queued_withdrawal_wins_while_active_actor_is_revalidating_policy_before_start`:
  el actor real tiene el lease en un runtime/OS thread independiente y espera
  una barrera de revalidación de política. La UI puede retirar Requested sin
  tomar ese lease; al continuar, el actor reconoce el CAS perdido como retirada,
  conserva el recibo exacto y no ejecuta hooks, comandos ni eventos ficticios.
- `known_stdin_result_and_full_hook_output_survive_uncertain_post_hook_and_reopen`:
  pre-hook real con 32 KiB de stderr íntegro, stdin conocido, post-hook real que
  falla y preserva stdout/stderr. La incertidumbre posterior conserva el
  recibo conocido del comando y se mantiene al reabrir el store.
- `host_observer_failure_after_known_effect_stops_before_post_hook_and_never_repeats_stdin`:
  falla la entrega al host tras guardar el efecto; queda Started con ese recibo,
  el post-hook no empieza y un retry exige verificación. Reconocerlo no repite
  stdin ni el pre-hook.
- `killed_real_actors_reopen_each_effect_boundary_without_restoring_or_repeating_processes`:
  SIGKILL del actor sin destructores tras Requested, antes del primer pre-hook,
  antes del comando, después del efecto conocido/antes de post-hooks y durante
  un pre-hook ya iniciado con efecto externo visible. El manager nuevo no tiene
  los procesos anteriores; rechaza Requested de la generación vieja. Started
  conserva su estado y bloquea procesamiento hasta reconocimiento explícito.

El observer opcional del actor recibe sólo snapshots cuya escritura y
sincronización finalizaron. Un error del observer detiene la continuación;
no transforma un efecto en pendiente reejecutable. Los tests usan ese contrato
para barreras reproducibles previas al siguiente efecto. También verifican que
la invocación de hooks dice `source = manual_command`, lleva los IDs humanos y
no inventa turn/call IDs de modelo.

Estas pruebas no aceptan resize/ConPTY en Unix, no verifican aislamiento de otra
plataforma y no prueban la presentación gráfica ni el consumo durante streaming
de modelo. Esos criterios requieren sus aceptaciones de manager, Runner/Resume
y UI correspondientes.


## Consumo durante Runner/Resume y presentación

`NativeManualCommandActor` está conectado al runner padre y al ResumeWorker del
desktop. Comprueba store/lease/owner y revalida la política guardada antes de
cada spawn de hook; se conserva el registry confirmado, sin adoptar cambios
externos durante la acción. El resultado conocido del comando queda guardado
antes de post-hooks. EOF/terminate no invalidan ese resultado por cerrar el
transporte; un cambio posterior de permisos/hooks detiene los hooks restantes.
Los diagnósticos conservan las observaciones acotadas del protocolo actual de
hooks; streams que no sean UTF-8 usan la conversión textual existente con
reemplazos, no un recibo binario nuevo.

La cola se atiende bajo el mismo lease por Notify durante el stream de modelo,
en límites entre grupos completos de herramientas y antes de cerrar el turno.
Durante una herramienta/hook/permiso ya en curso espera su próxima frontera
segura. La cerca transaccional permite reintentar únicamente el append bloqueado
por una nueva intención; nunca repite el modelo ni el efecto. Sin actor, Resume
exige cola vacía. El ResumeWorker bloquea incertidumbre antes de preparar MCP;
Requested con actor espera la validación normal de ruta/catálogo para consumirse.
Un Resume idle sí consume o rechaza las intenciones antes de indicar Complete.

`native_manual_commands` demuestra cuatro casos con servidor HTTP SSE local y
procesos reales: stdin durante streaming, cancelación previa con tail aceptado,
reapertura que rechaza generación anterior y exige reconocimiento de Started, y
Resume que entrega stdin+EOF durante streaming con un solo request/response.
No fabrica ToolStarted/ToolFinished para esos controles humanos.

Actividad ofrece Controlar comando y el historial aun con el manager ausente.
La vista prepara una confirmación con scope, generación y bytes originales;
ningún callback envía input directamente al proceso. TextArea conserva CRLF/CR,
Unicode y NUL sólo cuando se solicita preserve_line_endings. El historial muestra
input, resultado del comando, outcome agregado, diagnósticos y fechas completas
con scroll. Cancelled/Finished se aplican desde ACK durable y una respuesta vieja
no retrocede la revisión. Dismiss sólo oculta la vista. Un Started con actor vivo
se presenta en curso y no habilita reconocer recuperación; tras la interrupción,
la nota humana y CAS requieren un lease idle y no restituyen un proceso.

`native_command_control_ui` tiene diez pruebas de render/acciones, raw paste,
historia reabierta y recibos completos a 360/900 px. Los tests host
`native_command_control_app_tests` ejecutan un binario Rust real para verificar
paste→prepare→confirm→lease→bytes/EOF/terminate, permisos guardados frescos,
retirada de cola con lease ocupado y reconocimiento por revisión al reabrir sin
manager. `raw_text_area_tests` comprueba cuatro casos de clipboard/offsets raw y
la normalización predeterminada de los otros editores.

Estas pruebas no aceptan Windows ConPTY/MXC manual, inferencia de una cuenta
real, ejecución gráfica eframe en cada plataforma ni restauración de procesos.
Tampoco añaden output incremental durable, controles a workers hijos/automations
ni ejecución concurrente con una herramienta que ya está usando el proceso.
