# Aceptación opt-in con proveedores reales

## Ciclo local reproducible

`crates/openide-harness/tests/native_http_cycle.rs` prueba el recorrido sin claves ni red externa mediante un servidor HTTP loopback que entrega respuestas SSE reales al adaptador Chat Completions. Se ejecuta con:

```powershell
cargo test -p openide-harness --test native_http_cycle
```

El caso `plan_build_uncertain_effect_reopens_resumes_and_replays_checkpoint` completa Plan, cierra y reabre `SessionStore`, y pasa ese historial persistido a Build. Build recibe una llamada de herramienta por streaming; el ejecutor agrega un recibo al disco y pierde su confirmación. El test inyecta un fallo al guardar el evento terminal para representar una interrupción tras el `ToolStarted` ya sincronizado. Después de otra reapertura, `ResumeEngine` detecta `ToolOutcomeUncertain`, se niega a ejecutar la herramienta otra vez y sólo continúa cuando se registra manualmente el resultado verificado. La continuación del modelo vuelve a usar SSE. A continuación `CheckpointCoordinator` obtiene un resumen por HTTP con herramientas deshabilitadas, publica el archivo y su marcador, y el test reabre ambos stores para comprobar el replay del checkpoint. El archivo de efectos contiene exactamente un recibo antes y después de la recuperación. El backend de este caso envía un tope explícito `max_completion_tokens=4096`; después de reabrir el store, dos intentos con tope cambiado a 2048 o retirado deben fallar con `ContextMismatch`, sin eventos nuevos ni repetir la herramienta. Con el tope original, la continuación SSE y la compactación conservan el mismo campo HTTP. Esta aceptación verifica transporte y recuperación locales; no demuestra que una cuenta real admita el parámetro para un modelo particular.

El servidor tiene plazos breves y peticiones guionadas; un pedido inesperado de repetición falla el test. La interrupción inyectada es un fallo de journal controlado, no una terminación real del proceso. Este test cubre el protocolo Chat Completions, mientras que la aceptación opt-in de abajo prueba también Responses y los proveedores externos. `native_http_cycle` incluye además un caso de stream truncado que comprueba que una llamada de herramienta aparentemente completa no se ejecuta sin `[DONE]`.

## Cortes reales de proceso y reapertura

`process_cuts_reopen_plan_build_resume_and_checkpoint_without_repeating_effects`
en `native_http_cycle.rs` ejecuta catorce escenarios básicos y, en Unix, ocho
fronteras adicionales de hooks, con Plan por HTTP SSE y Build
admitido de forma durable. Un proceso hijo ejecuta el runner y termina con
`std::process::exit(86)`, sin ejecutar destructores, en cada frontera:
admisión, `TurnStarted`, `SystemInstructionsCommitted`, `ModelContextCommitted`,
`ModelRouteCommitted`, `ModelRequestStarted`, antes y después de
`ModelResponseCommitted`, `PermissionRequested`, `PermissionResolved`,
`ToolStarted`, recibo sincronizado en disco, `ToolFinished` y `TurnCompleted`.
El padre exige ese código de salida, limita la espera del hijo a veinte
segundos, abre un nuevo `SessionStore` y adquiere el lease abandonado.

Cada escenario comprueba el estado de replay esperado. Una herramienta iniciada
sin resultado permanece incierta: `run_until_pause` no agrega eventos ni ejecuta
el efecto. El operador de la prueba verifica el disco; cuando el corte precedió
al efecto, realiza explícitamente la operación y registra su resultado. Resume
recupera el mismo permiso pendiente o utiliza la decisión ya persistida. El
archivo final contiene exactamente un recibo y el journal exactamente un
`ToolStarted`. La compactación solicita un resumen HTTP con herramientas
deshabilitadas; después se cierran y reabren ambos stores y se verifican el
checkpoint activo y la conservación del plan.

La prueba usa el adaptador Chat Completions y respuestas locales guionadas.
No prueba pérdida de energía, journal parcialmente escrito, SIGKILL durante un
syscall, cortes durante la publicación del candidato/marker de checkpoint (cubiertos
por la prueba específica de la siguiente sección),
efectos alojados del proveedor, MCP, subagentes, UI ni modelos externos. Los
hooks tienen ocho fronteras adicionales descritas a continuación.
El corte antes del commit de respuesta puede repetir una petición de modelo;
no autoriza repetir una herramienta local iniciada. El helper
`native_process_cut_child` está ignorado en la suite normal y sólo se invoca
explícitamente desde el padre.

Verificación en Linux/NixOS el 29 de septiembre de 2026 con Rust 1.95.0: los once
escenarios pasaron. No había claves de OpenAI, DeepSeek, OpenRouter, Groq,
Mistral ni xAI, ni modelos de aceptación configurados en el entorno del proceso;
no se hicieron solicitudes de inferencia a proveedores externos. Esa ausencia no cuenta
como aceptación real aprobada. La consulta posterior de metadatos públicos de
OpenRouter se registra en la siguiente sección.

### Checkpoint durante Build activo (2026-09-30)

`active_checkpoint_acceptance::active_build_checkpoint_process_cuts_reopen_and_resume_http_without_repeating_tool`
añade el orden que no cubría el ciclo anterior: Plan HTTP y reopen, Build HTTP,
permiso explícito, herramienta con recibo sincronizado y resultado confirmado,
compactación HTTP de sólo el prefijo Plan terminado mientras Build sigue activo,
checkpoint, corte real, nuevos stores/lease y Resume HTTP explícito.

Los dos subprocesos terminan sin destructores: el primero deja el marcador de
checkpoint como último evento antes de guardar el request del paso 2; el segundo
muere después de su primer delta y el servidor retiene el terminal. Replay
conserva respectivamente ReadyForModel o ModelInFlight. Abrir/inspeccionar no pide
al modelo ni modifica el journal. La continuación conserva el contexto admitido
aunque el host ofrezca otras instrucciones, la ruta exacta, el resultado de la
herramienta, el plan y el goal; no repite efecto, permiso ni petición durable.
El prefijo parcial permanece en el journal y se excluye del historial autoritativo.
Un nuevo reopen termina Idle con el mismo checkpoint y ledger sin evidencia
inventada. El helper `active_checkpoint_cut_child` sólo lo ejecuta el padre.

La suite local completa pasó cinco pruebas y tres helpers ignorados. El
presupuesto de la fixture es explícito; no certifica los límites de un modelo
externo. Evidencia en Linux, sin claves, GUI, sandbox de otras plataformas,
pérdida de energía ni interrupciones dentro de syscalls.

### Hooks Pre/Post con proceso terminado

En Unix la misma aceptación añade `pre_hook_started`,
`pre_hook_effect_synced`, `pre_hook_diagnostic`, `pre_hook_finished`,
`post_hook_started`, `post_hook_effect_synced`, `post_hook_diagnostic`
y `post_hook_finished`. Instala dos hooks reales de
comando para `append_receipt`. El shell ejecuta un helper Rust separado que
lee el JSON de invocación, verifica fase/herramienta, agrega un recibo a
`pre_tool_use.log` o `post_tool_use.log` y llama a `sync_all`.

En las fronteras Started/diagnóstico/Finished, el journal termina el runner con exit 86
inmediatamente después del append sincronizado. En las fronteras del efecto,
el helper envía SIGKILL al PID del runner que lanzó el padre de la prueba,
únicamente después de sincronizar su recibo; el padre exige esa señal de salida.
No se ejecutan destructores del runner. Los argumentos y el ejecutable del hook
son idénticos durante recuperación; el selector del corte reside sólo en el
workspace de la fixture.

La reapertura verifica un solo HookStarted de la fase elegida y HookFinished
sólo en la frontera de resultado. El replay conserva ToolOutcomeUncertain en
los ocho casos. Resume no agrega eventos ni ejecuta automáticamente el hook o
la herramienta. Si el corte precedió al efecto de la herramienta, el operador
verifica su ausencia y lo realiza explícitamente. Luego registra el resultado
verificado, completa el turno por HTTP, compacta y reabre los stores. Los recibos
de hooks permanecen idénticos durante Resume: los confirmados contienen
exactamente una línea y los no ejecutados siguen ausentes. No se inventa un
resultado para un hook que no alcanzó su efecto.

Los ocho casos pasaron en Linux/NixOS el 2026-09-29. El helper
`native_hook_receipt_child` está ignorado y sólo se invoca desde el proceso de
hook de la fixture. No hay aceptación equivalente de estos cortes en
Windows/macOS, ni muerte durante las escrituras/syncs, ni aceptación con modelo
externo. Verificar manualmente la herramienta no ejecuta los hooks que quedaron
sin comenzar; el operador es responsable de verificar también sus efectos.

### Presentación de hooks y verificación

El transcript ahora conserva los eventos HookStarted/HookFinished que antes
ignoraba. Muestra ID del hook, ID de llamada, fase Pre/Post, decisión confirmada
o resultado pendiente/incierto. Después de reabrir un turno incompleto, un hook
sin HookFinished se presenta interrumpido y pide verificar su efecto; un
ToolFinished registrado manualmente no cambia ese hook a éxito.

La tarjeta de verificación toma los hooks del turno actual y de la llamada
exacta desde el journal, sin atribuir hooks de turnos anteriores. Las pruebas
`hook_activity_survives_store_reopen_and_manual_tool_resolution` y
`pending_hook_changes_from_live_to_uncertain_after_interruption` verifican
persistencia y transiciones. La prueba de UI
`hook_effect_details_and_verification_actions_remain_visible_on_small_surfaces`
verifica textos y accesibilidad por scroll de confirmar/dejar pendiente y del
campo de resultado a 360×320 y 760×720 píxeles. Los fallos de lectura de hooks
durante recuperación se muestran como errores de persistencia.

Estas pruebas usan eventos locales y el framework de UI propio; los ocho cortes
de proceso del apartado anterior verifican efectos y recovery del harness. No
constituyen una aceptación conjunta con un modelo externo ni una prueba visual
del sistema operativo. El nuevo evento `HookDiagnosticCommitted` guarda razón y stdout/stderr de un
hook completado, o el fallo conocido de una ejecución incierta, antes de
HookFinished. Los datos son diagnósticos: no cierran el hook ni autorizan repetir
su efecto. Replay verifica identidad/posición, un solo diagnóstico por hook,
razón/error de hasta 8192 bytes y salida combinada de hasta 256 KiB. El proceso
de hook conserva su límite de captura previo de 64 KiB; el límite del journal
permite la expansión UTF-8 de stderr decodificado con reemplazos.

La aceptación verifica los valores exactos de razón/stdout/stderr del comando
real en las fronteras de diagnóstico y resultado; cortar tras diagnosticar deja
ToolOutcomeUncertain y requiere verificación manual. El test de replay rechaza
diagnósticos duplicados, ajenos o sobredimensionados y conserva compatibilidad
con una secuencia anterior sin diagnóstico. La proyección reabre los campos y
las tarjetas UI los muestran en áreas desplazables acotadas. La razón y salida
no se presentan como instrucciones ejecutables.

Los lectores conservan un prefijo combinado de stdout/stderr hasta el límite
configurado, incluso ante timeout, cancelación, salida no exitosa, JSON inválido
u overflow. Tras detener el árbol del proceso, cada lector tiene hasta dos
segundos para drenar los bytes ya disponibles antes de abortarlo. Las pruebas
`timeout_after_side_effect_is_uncertain`, `cancellation_after_launch_is_uncertain`,
`failed_process_retains_both_diagnostic_streams` y
`oversized_output_is_bounded_and_uncertain` ejecutan procesos reales y comprueban
ambos streams y el límite agregado. La prueba del runner
`post_hook_timeout_after_tool_effect_requires_manual_resolution` verifica que el diagnóstico parcial sobrevive al reopen del
store sin HookFinished ni ToolFinished. Runner y Resume usan el mismo resultado
de captura y persisten el diagnóstico antes de reportar incertidumbre. Los
lectores tienen un guard que los aborta al abandonar la ejecución, incluso si
las tuberías heredadas no reciben EOF; `dropped_hook_readers_release_open_pipes_without_eof`
verifica que se liberan aunque los escritores sigan abiertos.

El orden de los bytes entre streams no se reconstruye y el prefijo retenido al
agotar el límite puede depender de la planificación. Si muere el runner con
SIGKILL antes del append durable, puede no existir diagnóstico; tampoco se
garantizan bytes emitidos después del plazo de drenaje por descendientes que
escapen a la contención. La ausencia de salida no significa ausencia de efecto.

## Publicación de checkpoints interrumpida

`checkpoint_coordinator::tests::checkpoint_publication_process_cuts_keep_last_committed_history_after_reopen`
ejecuta tres procesos hijos sobre stores reales con un checkpoint previo y un
segundo turno completo. Termina el proceso sin destructores después del resumen,
después de publicar el candidato y después de sincronizar el marcador en el
journal. Los puntos de corte están compilados sólo en el binario de tests.

Tras reabrir ambos stores y recuperar el lease, la prueba comprueba que el resumen
sin candidato no altera el checkpoint previo; un candidato sin marcador queda
huérfano y no sustituye el historial; y un marcador sincronizado activa exactamente
el nuevo checkpoint, aunque el worker no alcanzara la verificación final. En las
dos primeras fronteras, una nueva compactación conserva el segundo turno y
publica correctamente su checkpoint. Se verifican los números de candidatos y
marcadores, la secuencia cubierta, el historial y el estado de replay.

Los tres escenarios pasaron en Linux/NixOS con Rust 1.95.0 el 2026-09-29.
El resumen de esta prueba usa un backend Rust local; el transporte HTTP de
compactación se cubre en `native_http_cycle`. Aún no se prueba terminación durante
un `write`, `rename` o `sync_all`, pérdida de energía ni durabilidad física del
almacenamiento. No se extrapola el resultado a Windows/macOS.

## Metadatos reales sin credenciales de inferencia

El 2026-09-29 se ejecutó explícitamente
`cargo test -p agents-window live_openrouter_catalog_exposes_exact_limits_and_reopens_cache -- --ignored --nocapture`.
La prueba usa el cliente propio y el registro declarado para consultar directamente
`https://openrouter.ai/api/v1/models?output_modalities=all`, sin clave de API,
sin seguir redirects y con límites de tiempo/tamaño. Guarda el cache y lo reabre
por `load_cache`, que revalida la definición y credencial. Ambas corridas pasaron.
La segunda informó 635 IDs exactos y 393 IDs con herramientas y un par válido de
contexto/salida resoluble para ejecución. Los tres primeros pares observados:

- `aion-labs/aion-2.0`: contexto 131072, salida máxima 32768.
- `aion-labs/aion-3.0`: contexto 131072, salida máxima 32768.
- `aion-labs/aion-3.0-mini`: contexto 131072, salida máxima 32768.

Estos valores son una observación fechada de la API, no constantes del programa
ni una lista de modelos autorizados. La prueba también comprueba que un ID
inexistente no hereda presupuesto. Permanece ignorada en la suite normal porque
requiere red externa; su ejecución explícita sí se cuenta como aceptación de
consulta, parseo y reapertura de metadatos. No hubo generación, llamadas de
herramientas de un modelo real, cobros, comprobación de acceso de cuenta ni
verificación de los límites de todos los proveedores detrás del router. Las
pruebas de inferencia real de OpenAI/DeepSeek siguen pendientes por falta de
credenciales/modelos configurados.

## Proveedores externos

El 2026-09-30 se volvió a comprobar únicamente la presencia de variables en el
proceso de trabajo: no estaban disponibles `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`,
`OPENROUTER_API_KEY`, `GROQ_API_KEY`, `MISTRAL_API_KEY` ni `XAI_API_KEY`, ni los
IDs `OPENIDE_OPENAI_TEST_MODEL` / `OPENIDE_DEEPSEEK_TEST_MODEL`. No se imprimieron
valores ni se hicieron requests de inferencia externos. Esta observación no
describe las credenciales de otra sesión del desktop ni el vault de otro host.
La aceptación externa sigue pendiente; las pruebas locales de streaming y
recuperación continúan de forma independiente.

`crates/openide-harness/tests/real_provider_acceptance.rs` comprueba el transporte nativo con los adaptadores reales de OpenAI Responses y DeepSeek Chat Completions. Las pruebas están marcadas `ignored` y además exigen `OPENIDE_REAL_PROVIDER_ACCEPTANCE=1`. Si faltan la clave o el modelo de un proveedor, su prueba termina sin hacer una petición.

Configura sólo los proveedores que quieras probar. El modelo debe ser un ID con herramientas de función habilitadas en tu cuenta; no hay un modelo predeterminado para estas pruebas.

| Proveedor | Clave | Modelo |
| --- | --- | --- |
| OpenAI Responses | `OPENAI_API_KEY` | `OPENIDE_OPENAI_TEST_MODEL` |
| DeepSeek Chat Completions | `DEEPSEEK_API_KEY` | `OPENIDE_DEEPSEEK_TEST_MODEL` |

En PowerShell, después de configurar las variables en la sesión actual, ejecuta:

```powershell
$env:OPENIDE_REAL_PROVIDER_ACCEPTANCE = "1"
cargo test -p openide-harness --test real_provider_acceptance -- --ignored --test-threads=1
```

Cada proveedor crea un workspace temporal con un único archivo de lectura. Plan pasa por `AgentRunner` con journal y ninguna herramienta ejecutada. Se cierra y reabre el store, se admite Build y `ResumeEngine` recupera ese turno desde `AdmittedNotStarted`. Build sólo anuncia `read_file`, verifica su resultado y el marcador en la respuesta, y vuelve a abrir el journal para validar el replay. El adaptador OpenAI también comprueba la persistencia del ID y de los elementos originales de Responses. Los límites son tres pasos de modelo y una llamada de lectura para Build; las peticiones pueden generar costos.

Los tests no imprimen claves, prompts, respuestas ni eventos. Los endpoints son los oficiales fijados por los adaptadores del producto. El workspace y su journal temporal se eliminan al terminar la prueba. Un fallo sólo informa la fase; para investigar datos del proveedor se necesita un entorno de depuración privado.

Esta prueba cubre la recuperación de un turno **admitido pero aún no iniciado** y la continuación desde el historial de un turno completo tras reabrir el store. No simula una caída durante una petición HTTP, un efecto de herramienta incierto, compactación, UI ni subagentes. El éxito requiere que el modelo configurado coopere con una llamada de lectura; no se reintenta una respuesta inesperada.

### Anthropic Messages: aceptación externa preparada

`crates/openide-harness/tests/native_anthropic_live.rs` agrega
`anthropic_messages_plan_build_read_and_journal_recovery`. Está ignorada en la
suite habitual y requiere conjuntamente `OPENIDE_REAL_PROVIDER_ACCEPTANCE=1`,
`ANTHROPIC_API_KEY` y `OPENIDE_ANTHROPIC_TEST_MODEL`. El modelo es un ID exacto
elegido por el operador, sin predeterminado ni inferencia por prefijos. El
adaptador exige que el ID de la respuesta coincida; un alias que el servicio
resuelva a otro ID no cumple esta aceptación. El endpoint está fijado en
`https://api.anthropic.com/v1`, usa `x-api-key` y la versión `2023-06-01`, y no
sigue redirects. La ausencia o invalidez de una variable produce un fallo
explícito antes de crear el cliente o pedir HTTP; no cuenta como prueba aprobada.

Después de configurar esas variables en la sesión actual, ejecutar únicamente:

```powershell
cargo test -p openide-harness --test native_anthropic_live anthropic_messages_plan_build_read_and_journal_recovery -- --ignored --exact --nocapture
```

La prueba consulta `count_tokens` para un pedido de texto con instrucciones y
la definición de `read_file`, y exige un contador positivo. Luego completa Plan
mediante el runner nativo sin ejecutar herramientas, reabre el store, admite
Build, vuelve a reabrir y lo recupera desde `AdmittedNotStarted`. Build anuncia
únicamente una lectura local, comprueba un marcador aleatorio y conserva el
archivo. Un reopen final valida el journal y un nuevo Resume Idle no puede
contactar al proveedor ni agregar eventos.

Cada respuesta debe conservar ID, uso y su envoltorio propio con modelo y digest
del endpoint. Se comprueba la correspondencia ordenada entre bloques de texto,
llamadas de herramienta e historial canónico. Si el proveedor informa thinking
o redacted thinking, también se exige conservar firma o datos originales; la
continuación real recibe exactamente esos elementos persistidos junto con el
resultado de la lectura. No se activa thinking ni un esfuerzo cuya capacidad
no haya sido consultada para ese modelo. La prueba conserva el perfil Messages
en los marcadores de ruta y contexto. No imprime credenciales, prompts,
respuestas, firmas ni eventos.

El tope `max_tokens=4096` es una política del pedido, no una capacidad publicada
del modelo. Esta aceptación no consulta capacidades o límites de cuenta y usa
`TokenBudgetPolicy=None` explícitamente. El contador positivo no certifica una
ventana ni un máximo de entrada; el preflight con límites independientes,
rechazos y compactación se verifica en `native_anthropic_compact.rs` mediante
HTTP local. Tampoco prueba imágenes externas, cortes de proceso externos,
compactación con el proveedor, UI, hooks, MCP, comandos o subagentes. Exige la
cooperación del modelo con una única lectura y puede generar costos.

El 2026-09-30 se comprobó sólo la presencia de variables en este proceso:
faltaban `ANTHROPIC_API_KEY`, `OPENIDE_ANTHROPIC_TEST_MODEL` y
`OPENIDE_REAL_PROVIDER_ACCEPTANCE`. No se imprimieron valores ni se hicieron
peticiones externas. El caso está preparado; la aceptación de inferencia
Anthropic real continúa pendiente. La ejecución habitual compiló el caso y
lo dejó ignorado (`0 passed, 1 ignored`); clippy estricto del harness pasó.
También se invocó explícitamente el caso con opt-in activo y la clave forzada a
ausente: terminó con el fallo esperado en la guarda de `ANTHROPIC_API_KEY`,
antes de construir el cliente (`0 passed, 1 failed`, salida 101). Esa verificación
de la guarda tampoco cuenta como aceptación del proveedor.
Los contratos de transporte y contador
se basan en las APIs oficiales de
[Messages](https://platform.claude.com/docs/en/api/messages/create) y
[count_tokens](https://platform.claude.com/docs/en/api/messages/count_tokens).


### Controles humanos: alcance de las pruebas del 30 de septiembre

Las aceptaciones nuevas de comandos humanos usan HTTP SSE local, procesos Rust/
shell reales y stores reabiertos. No son aceptación de inferencia externa.
Se comprobó sólo la presencia de nombres de variables en este proceso, sin leer
ni publicar valores: faltan OPENAI_API_KEY, DEEPSEEK_API_KEY, OPENROUTER_API_KEY,
GROQ_API_KEY, MISTRAL_API_KEY, XAI_API_KEY, ANTHROPIC_API_KEY, GOOGLE_API_KEY,
GEMINI_API_KEY y OPENIDE_API_KEY; también faltan los IDs de modelo de aceptación
OPENIDE_OPENAI_TEST_MODEL y OPENIDE_DEEPSEEK_TEST_MODEL. Esto no afirma que el
usuario carezca de cuentas o credenciales guardadas en otro entorno.
El trabajo independiente de ledger, actor, UI, hooks y Resume continúa con
fixtures controlados. Ver native-command-operations.md para evidencia y límites.


## Codex: diagnóstico inicial de Plan fallido (2026-09-30)

Se encontró un record OAuth privado de Codex en el root real de la app, aunque
las claves API de las aceptaciones anteriores no están en el entorno. La prueba
optativa `app::native_codex_live_app_tests::saved_codex_plan_uses_exact_production_factory_and_reopens_durable_native_history`
usa `OPENIDE_CODEX_LIVE_ACCEPTANCE=1`, el ID/endpoint de producción guardados y
la fábrica propia del desktop. Copia la credencial a un temporal privado sólo
cuando quedan al menos 600 segundos de vigencia; no permite refresh/rotación.
El workspace y SessionStore son temporales, sin abrir el historial personal.

Dos ejecuciones usaron `gpt-5.6-luna` en `https://api.openai.com/v1`. Cada una
registró una llamada lógica y cero respuestas comprometidas. La segunda
conserva un TurnFailed y diagnóstico `protocol_sse`, sin status HTTP conocido;
ese rótulo agrupa los fallos de parsing/cierre de Responses y no demuestra una
causa específica ni una denegación de credencial. Reabrir el journal conservó
los eventos. Perfil, registro y credenciales personales permanecieron
idénticos. Ambas ejecuciones **fallaron la aceptación de Plan**; no se cuentan
como generación verificada ni como Plan → Build → checkpoint → Resume.

La solicitud usa un paso de modelo, cero efectos de herramientas y plazo de
120 segundos. El transporte existente puede efectuar hasta tres intentos
transitorios antes de texto observable; no se afirma un único POST físico.
401/403 son terminales cuando el adaptador observa ese status. Sólo se conserva
un JSON 0600 de diagnóstico, sin cuerpo, journal, token ni datos de cuenta.
La prueba local del clasificador pasó; el diagnóstico nunca se usa como regla
de autorización o retry.

En ese momento quedaban por verificar el endpoint/autorización efectivos, el
catálogo consultado por la cuenta y el ciclo integral con ese modelo. La referencia
`references/codex@33a0f766` distingue URLs ChatGPT y API key; esa lectura no
acredita por sí sola el contrato de la ruta propia actual.

## Codex: aceptación real aprobada del streaming (2026-09-30)

Después de corregir la reconciliación de items `done` y terminal Responses,
se repitió explícitamente la prueba de Plan con `OPENIDE_CODEX_LIVE_ACCEPTANCE=1`.
Pasó en 11,17 s: un pedido lógico, una respuesta comprometida, turno completado y
journal reabierto. El modelo y endpoint fueron los guardados: `gpt-5.6-luna` y
`https://api.openai.com/v1`. Perfil, registro y credencial personales permanecieron
idénticos. Esa ejecución sustituye el resultado fallido de Plan como evidencia
actual; los dos fallos anteriores permanecen registrados arriba.

También pasó
`app::native_codex_stream_live_tests::saved_codex_build_stream_commits_tool_before_execute_and_idle_resume_sends_nothing`
en 6,89 s y volvió a pasar en 15,43 s después de integrar el decoder SSE
incremental. Usa la misma fábrica de backend y una copia privada temporal de la
conexión OAuth con al menos 600 s de vigencia, sin refresh ni rotación. Expone
únicamente `read_file` sobre un archivo temporal de marcador; limita el turno a
dos pasos de modelo y una herramienta. El lector comprueba el journal durable
desde `execute`, antes de leer el archivo.

Se exigieron dos `ModelRequestStarted`, dos `ModelResponseCommitted`, un
`ToolCallReady`, una ejecución y un resultado de lectura confirmado, y un único
terminal de éxito con el marcador exacto. Al reabrir, el journal y la proyección
son iguales, replay es Idle y Resume no agrega eventos ni contacta al modelo o al
lector otra vez. Archivo temporal, perfil, registro y credenciales originales
permanecieron iguales. No se abrieron conversaciones privadas como artefactos.

Comandos de aceptación explícita:

```sh
OPENIDE_CODEX_LIVE_ACCEPTANCE=1 cargo test -p agents-window saved_codex_plan_uses_exact_production_factory_and_reopens_durable_native_history -- --ignored --nocapture --test-threads=1
OPENIDE_CODEX_LIVE_ACCEPTANCE=1 cargo test -p agents-window saved_codex_build_stream_commits_tool_before_execute_and_idle_resume_sends_nothing -- --ignored --nocapture --test-threads=1
```

Estas pruebas verifican inferencia, streaming, validación antes de la lectura,
commit, reapertura y ausencia de efectos repetidos. No certifican todo el catálogo,
precios, límites de cuenta, herramientas de escritura, compactación remota, MCP,
browser, comandos, subagentes ni cortes de proceso con ese proveedor. Esos
contratos tienen fixtures locales independientes. No se informa un costo ni
una latencia general a partir de estos dos turnos.

### Cabecera SSE ausente: aceptación del 1 de octubre

Se reprodujo el fallo de JSON de la captura: HTTP 200, sin `Content-Type`, con
cuerpo SSE. Tras añadir detección incremental cuando la cabecera falta o es
incorrecta, la misma prueba Build pasó en **20,49 s**, usando el modelo guardado
`gpt-5.6-luna` y la fábrica de producción. Conservó eventos tipados, items opacos,
commit de respuesta anterior a la lectura, journal reabierto y Resume Idle sin
efectos o pedidos repetidos. La copia privada de OAuth no realizó refresh ni
rotación. No se guardaron cuerpos, tokens ni conversaciones personales como
artefactos de diagnóstico. Los fixtures HTTP verifican además que una respuesta
mal etiquetada y truncada se rechaza antes de ejecutar herramientas.
