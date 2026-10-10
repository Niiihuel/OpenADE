# Handoff: visual de proveedores y catálogo

## Alcance actualizado por el usuario · 30 de septiembre de 2026

La UX actual de conexiones expone **Codex, OpenRouter, OpenCode Zen/Go y NVIDIA
NIM**. El usuario pidió retirar del catálogo visible la ruta personalizada,
`gpt-4.1` y la configuración de `OPENIDE_API_KEY`. Codex y OpenRouter usan OAuth
desde el navegador; Zen/Go y NIM usan las claves documentadas por esos servicios.
El registro abierto y los adaptadores anteriores se conservan en el motor para
compatibilidad de sesiones. Esta selección visible de cuatro proveedores tiene
prioridad sobre los párrafos anteriores de este handoff que pedían mostrar todo
el registro. El flujo de Codex usa Sign in with ChatGPT y Responses públicas en
el harness propio; no requiere lanzar el agente CLI. La ruta de plan no admite
`max_output_tokens`, por lo que no debe ofrecer el editor de topes de salida.
Se está editando `settings_ui/providers.rs` y los bloques de conexión de `app.rs`
en esta sesión: conservar los ajustes ajenos al conectar estos flujos.

### Límite entre presentación y recuperación

El usuario confirmó también en la sesión del harness la selección visible de
estos cuatro proveedores. El filtro pertenece a las rutas del selector y las
páginas de conexión. No vaciar `NativeProviderSettings` ni la preferencia
guardada de modelo/esfuerzo cuando un proveedor queda fuera de ese filtro:
esa identidad se usa para recuperar stores, checkpoints e hijos. Tampoco
filtrar por esos cuatro nombres `model_catalog::official_specs`, discovery ni
las fábricas del motor. Una ruta histórica oculta no aparece como fallback
en el chip; el estado vacío y «Conectar proveedor» siguen siendo visibles.

Estado observado el 2026-09-30 en `codex/ui-component-quality`, dentro de
`/home/nihuel/projects/personal/openADE`. El árbol contiene trabajo local del
harness aún sin publicar: la referencia para continuar es el worktree actual,
además de los documentos, y no solamente el último commit remoto.

### Contrato actual del composer

Las tareas nuevas usan exclusivamente el harness nativo. El selector expone
sólo rutas con conexión disponible; un catálogo público o cacheado no convierte
una credencial ausente en conexión. Un adaptador registrado que admite conexión
sin clave conserva su endpoint/modelo explícitos válidos, incluso después del
descubrimiento. No hay modelo de fallback visible ni envío desde el perfil
predeterminado implícito.

El estado vacío dice «No hay modelos disponibles» (chip compacto: «Sin modelos»).
Todas las vistas del picker mantienen «Conectar proveedor», conectado a
Settings → Proveedores y modelos → Conexión. La navegación preserva borrador y
ruta y no crea un turno. «Avanzado» y esfuerzo no aparecen para una selección
obsoleta; Stop sigue disponible durante ejecución.

Contexto tiene una tarjeta inicial de hasta 320 × 200 px, con porcentaje,
cociente, barra y sólo entrada/salida. Las categorías del motor están en «Ver
detalles» y conservan sus unidades reales; no inventar segmentos de tokens para
bytes de skills o MCP. Los contratos están en `native-context-projection.md`,
`settings-native-ux.md` y `native-streaming.md`.

## Instrucciones listas para el otro agente

> Trabajá en la presentación visual de proveedores, modelos y catálogo de
> OpenIDE, usando el framework Rust propio `openide-ui`. Leé primero las
> instrucciones aplicables de AGENTS.md, `/home/nihuel/.codex/RTK.md`,
> `docs/providers-models-architecture.md`, `docs/native-harness-roadmap.md` y
> `docs/ui-provider-catalog-handoff.md`.
>
> El motor ya está conectado: reutilizá los datos, acciones y servicios actuales
> de `ViewState`, el composer y Settings. Implementá la mejora visual y comprobá
> sus acciones reales. Preservá los contratos que detalla este documento.
> Mostrá estados de carga, credencial ausente, error, caché y capacidades
> desconocidas con información honesta. Los proveedores deben surgir del
> registro abierto y los modelos de las fuentes directas de cada proveedor.
>
> Priorizá `settings_ui.rs`, `composer_ui.rs` y los componentes de
> `crates/openide-ui`. Si necesitás cambios en campos o acciones de `app.rs` o
> `view.rs`, prepará cambios localizados y coordiná esos bloques con el agente
> que sigue implementando el harness. Esos archivos tienen modificaciones
> activas de recuperación, subagentes y permisos. Conservá el trabajo local
> existente y las pruebas que cubren los contratos.
>
> Verificá superficies pequeñas y grandes, scroll, foco, teclado, accesibilidad
> y acciones sobre servicios reales. Ejecutá `cargo test --workspace`,
> `cargo fmt --all` y `cargo clippy --workspace --all-targets` según el entorno
> de este repositorio. Informá los cambios, pruebas y limitaciones concretas.
> El otro agente mantiene el goal del harness y su aceptación; este trabajo
> visual no debe declarar ese goal completo.

## Qué está conectado y dónde

- `apps/agents-window/src/provider-definitions.json`: definiciones incorporadas.
  Un `provider-registry.json` en el directorio de datos agrega proveedores para
  los protocolos implementados; el conjunto es extensible.
- `native_provider.rs`: registro, resolución de conexiones y credenciales,
  descubrimiento autenticado, capacidades revisadas exactas, esfuerzos y
  configuración durable de topes de salida.
- `model_catalog.rs`: consultas directas, parser según mapas del registro,
  catálogo y caché con origen/fecha/error, límites por modelo exacto.
- `composer_selection.rs`: rutas efectivas y selección de modelo/esfuerzo.
- `app.rs`: refresh y workers, selección, resolvedor común de presupuesto,
  construcción del backend y acciones reales de configuración.
- `view.rs`: estado de presentación. Incluye `composer_routes`,
  `composer_provider_status`, `composer_effort_options`, los campos
  `native_output_limit_*` y el catálogo.
- `composer_ui.rs`: picker de modelo/proveedor y esfuerzo, controles del composer.
- `settings_ui.rs`: presentación y edición de proveedores y topes de salida.
- `crates/openide-harness/src/backend.rs` y `responses.rs`: transporte y
  ejecución nativa Chat Completions y Responses.
- `crates/openide-ui`: framework y componentes nativos propios que hay que usar.

OpenAI usa Responses; OpenRouter, DeepSeek, Groq, Mistral y xAI tienen adaptadores
Chat Completions. Hay conexión personalizada y registro extensible para los
protocolos implementados. Un listado exitoso no prueba acceso de generación ni
aceptación integral del adaptador con una cuenta externa.

## Contratos de datos que la presentación debe conservar

- La ruta se identifica por proveedor, ID exacto del modelo y protocolo.
  El nombre visible no reemplaza al ID ni agrupa capacidades por familia.
- Los modelos/API nunca eligen endpoints de inferencia, credenciales,
  permisos, herramientas ni un protocolo nuevo.
- `CatalogSource::ReviewedSeed` es respaldo local revisado;
  `OfficialApi` y `verified_at_ms` describen una comprobación real de listado.
  `attempted_at_ms` y `last_error` describen el intento, también si falló.
  La última lista válida puede seguir disponible con error de refresh.
- `DiscoveredModel::tool_call_declared` distingue un negativo explícito de un
  campo ausente. Un negativo impide la ruta del agente aunque exista respaldo
  revisado. La capacidad desconocida conserva su estado desconocido.
- Los límites `context_window_tokens` y `max_output_tokens` pertenecen al ID
  exacto. Los pares parciales no se completan mezclando fuentes. La UI consulta
  el resolvedor común del host para porcentaje/presupuesto, sin calcular una
  segunda política local.
- `supported_efforts` y el adaptador determinan el control y lo que se puede
  enviar. No se inventan niveles a partir del nombre del modelo/proveedor.
- El editor del tope de salida guarda `native-output-limits.json` por ruta
  exacta. Responses envía `max_output_tokens`; Chat exige la elección explícita
  de su parámetro. Hay validación, error visible y bloqueo durante ejecución.
  Ese tope participa del presupuesto y de la huella de Resume: no es decorativo.
- La clave vive en el mecanismo de credenciales, no en catálogo/transcript.
  El login de Codex CLI es otro perfil y no autentica el harness propio.
- Precio de referencia, acceso declarado, disponibilidad de clave, listado
  verificado y generación aceptada son datos distintos que la UI debe reflejar.

## Evidencia disponible y límites

El roadmap y `docs/native-real-provider-acceptance.md` contienen pruebas
identificadas. Hay HTTP local para transporte, permisos, recuperación y
persistencia; pruebas de caché, presupuesto, topes de salida, acciones y layout.
La consulta pública real a OpenRouter del 2026-09-29 verificó 635 IDs exactos y
393 pares de límites con herramientas, y reapertura del caché. Esa prueba no
realizó inferencia y no usó credenciales de una cuenta de generación.

El motor está conectado, pero siguen pendientes aceptación externa de
inferencia, más adaptadores/modelos verificados y otros criterios del harness.
La mejora visual debe mostrar esos límites y conservar los servicios reales.

## Integración observada durante el trabajo visual (2026-09-29)

En la última corrida compartida, estas cinco pruebas de la app fallaron:
`settings_keeps_shared_frame_radius_and_keyboard_actions`,
`model_catalog_uses_side_by_side_lists_only_when_settings_has_room`,
`mcp_settings_keeps_list_visible_and_opens_a_centered_scrollable_editor`,
`settings_routes_expose_their_existing_actions` y
`mcp_oauth_editor_and_authorization_state_are_visible`.
Los fallos observados fueron frame/radio/borde, alcance de `catalog_browse:openai`,
texto MCP Activado, `composer_add_context` en WorkspaceSearch y URL OAuth a 520 px.
El fixture de `session_surface.rs` ya conserva los nuevos campos del catálogo sin
el bloque duplicado que se introdujo por ediciones concurrentes. La omisión de
mailbox agrega un control funcional localizado en ese archivo y acciones en
`app.rs`; las 8 pruebas de mailbox pasaron. Revalidar la suite integrada después
de completar los cambios visuales; esta lista registra una observación y no
sustituye una nueva corrida.

Actualización posterior: la nueva corrida integrada de `cargo test --workspace`
pasó 1184 pruebas, con 11 ignoradas. Las cinco fallas anteriores ya no se
reproducen en esa corrida. Formato y diff check pasaron; Clippy terminó sin
errores y con 50 advertencias. La limpieza de temporales de subagentes agrega
un bloque localizado de recuperación en el constructor de `app.rs`; no cambia
los contratos del catálogo.

Observación posterior durante edición compartida: se corrigió un delimitador
sobrante en HeatmapChart y `UiTree::layout_at` ahora tiene visibilidad de crate
para las pruebas temporales de componentes hermanos. La regresión del rechazo
nativo dejó de aparecer en la última corrida. Esa corrida falla en
`composer_ui::tests::model_picker_preserves_exact_ids_and_distinguishes_unknown_from_unsupported`
(`true` frente a `false`, línea 2417 al observarlo). Revisar la distinción de
ruta desconocida frente a capacidad negativa explícita y repetir la suite;
no tratar desconocido como soporte verificado. Los goals durables nuevos viven
en core y todavía no cambian el contrato visual del composer.

Nueva verificación integrada: `cargo test --workspace` pasó 1202 pruebas, con
12 ignoradas, en 12 suites; fmt/diff check pasaron y Clippy terminó sin errores,
con 50 advertencias. El tooltip de cada ruta usa ahora un ID terminado en
`:description`, separado del ID `composer_route:` de selección, y la prueba de
Unknown/Unsupported volvió a pasar. También se actualizaron paths de `build_view`
en nuevas pruebas de la app y el fixture de `session_surface` para el campo
`github_contributions`. Los cambios de goals agregan un contrato core descrito
en `docs/native-goals.md`; la integración de esos estados en la UI sigue pendiente.

Verificación posterior de publicación/recuperación de goals: la suite integrada
pasó 1206 pruebas, con 13 ignoradas; fmt/diff check pasaron y Clippy terminó sin
errores, con 46 advertencias. El trabajo visual ya expone `UiTree::layout_at`
como API pública con reloj explícito; las pruebas de gráficos de la app compilan.
La limpieza de temporales nuevos de goals es core y conserva el contrato visual
actual; su conexión de startup/UI sigue pendiente en `docs/native-goals.md`.

La recuperación de goals ahora se ejecuta al iniciar la app mediante
`NativeGoalStore::recover_idle_sessions`. `app.rs` conserva un reporte tipado y
proyecta los errores de ese inicio en `sessions_status` sin cerrar el store. La
fila actual trunca la advertencia; un panel completo/retry de mantenimiento sigue
pendiente en el trabajo de goals. El framework agrega `Frame::text_layouts()`:
una copia de diagnóstico con fuente, runs, rectángulos y clipping, sin exponer
campos privados de selección. La prueba de wrapping MCP usa esa API.

Verificación posterior a la integración de startup de goals: workspace pasó
1212 pruebas, con 13 ignoradas, en 12 suites. Fmt/diff check pasaron; Clippy
terminó sin errores y con 46 advertencias. Incluye cambios visuales concurrentes;
no son todas pruebas nuevas del harness.

Estado actualizado de coordinación: el motor del catálogo sigue conectado a
Settings, composer y ejecución nativa. El agente del harness mantiene registro,
descubrimiento, capacidades exactas, presupuestos, transporte y recuperación.
El agente visual puede trabajar en presentación y componentes de openide-ui;
cualquier cambio de campos/acciones compartidos debe conservar estos contratos.
El adaptador nuevo `NativeGoalToolExecutor` vive en openide-harness: permite
consultar goals y registrar evidencia/completar en Build bajo autoridad del turno.
Su instalación en las rutas desktop y su presentación visual siguen pendientes;
no reemplaza todavía el contrato `composer-goals.json`.

Última verificación integrada: 1216 pruebas pasaron, con 13 ignoradas, en 12
suites; fmt/diff check pasaron y Clippy terminó sin errores, con 46 advertencias.
Incluye cambios visuales concurrentes. La aceptación externa con inferencia real
y los criterios pendientes del roadmap siguen abiertos.

El trabajo siguiente del harness agregó recibos de modificaciones de goals y dos
cortes reales después de su guardado, antes del resultado de herramienta. No
cambia datos/acciones visuales del catálogo. La consulta es una API del host;
instalación desktop y presentación de recuperación siguen pendientes.

Verificación de recibos integrada: workspace pasó 1217 pruebas, con 14 ignoradas,
en 12 suites. Los dos helpers de cortes de goals se ejecutan explícitamente desde
sus pruebas padre. Fmt/diff check pasaron; Clippy terminó sin errores y con 46
advertencias de estilo en la app. Las pruebas de inferencia externa siguen sin
contarse como aceptación cuando faltan credenciales.

Goals: el agente del harness conectó el adaptador de tools después de admitir
el turno padre y en ResumeWorker. Esto agrega capacidades reales de consulta en
Plan y evidencia/cierre en Build; no cambia campos ni acciones visuales del
catálogo. Los turnos anteriores conservan su schema de tools. El nuevo método
`certifies_plan_read` distingue lectura en Plan de ejecución concurrente, con
delegación por rutas locales en composites MCP. La UI de activación nativa, los
hijos y la presentación/reconciliación de recibos aún están pendientes.

Verificación integrada posterior a conexión padre/Resume y lectura Plan: 1221
pruebas pasaron, 14 ignoradas, en 12 suites. Formato/diff check pasaron y Clippy
terminó sin errores, con 46 advertencias existentes en agents-window.

El agente del harness también habilitó goal tools en workers de subagentes: cada
hijo usa su propio lease/admisión y conserva el catálogo anterior si su reserva
es previa. No se agregaron campos/acciones visuales del catálogo ni se copian
ledgers del padre. Activación UI y conciliación de recibos siguen pendientes.
La suite integrada pasó 1224 pruebas, con 14 ignoradas, en 12 suites.

El editor de Goal agregó controles funcionales localizados para activar goals
nativos con criterios explícitos y pausar/reanudar bajo lease/CAS. Archivos:
`native_goal_ui.rs`, campo `ViewState.native_goal`, proyección opcional en
ComposerProps y acciones scoped en app.rs. Home/session fixtures se adaptaron
al campo; no cambió el catálogo. La presentación nativa en el menú conserva el
editor previo de texto y no migra composer-goals.json. Se verificaron acciones
app/reopen y layout/pointer a 420/900 px; el visual completo a otros tamaños,
accesibilidad y flujo integral con modelo sigue pendiente. Workspace pasó 1227
pruebas, con 14 ignoradas, en 12 suites; formato/diff check pasaron.

La última verificación también terminó Clippy sin errores, con 46 advertencias
previas de estilo en agents-window; la nueva acción de criterios no agrega otra.

El harness agregó `goal_context.rs`: proyección íntegra de goal/criterios en las
instrucciones congeladas del turno, con el límite existente de 512 KiB y sin
recortar alcance. Resume usa el snapshot original; los hijos reconstruyen su
base mediante identidad de reserva y agregan sólo su goal propio. No se cambian
campos/acciones visuales del catálogo. Sigue pendiente el ciclo autónomo y la
aceptación integral UI→modelo→evidencia→checkpoint de goals.

Verificación integrada del contexto de goals: 1229 pruebas pasaron, 14 ignoradas,
en 12 suites. Fmt/diff check pasaron y Clippy terminó sin errores, con 46
advertencias existentes. No se cuentan modelos locales como aceptación externa
de proveedor ni como el ciclo autónomo completo.

Selección de objetivo: el goal nativo del ledger ahora prevalece sobre texto
anterior del composer en contexto y etiqueta, incluyendo Paused/Complete. El
texto anterior se conserva en disco; el hijo recibe sólo la base del proyecto.
Resume antes del primer contexto selecciona el objetivo dentro del worker bajo
lease, con datos de proyecto/legacy separados en ResumeWorkerRequest. Pasaron
73 pruebas filtradas por `native_` de agents-window, incluidos los nuevos casos.

Integración visual observada: se corrigieron conversiones Vec<&str>/String del
tooltip nuevo en tree.rs y se adaptó la referencia del test al helper
model_route_details. La corrida workspace siguiente falló en
`codex_account_status_appears_once_in_provider_heading` (espera Heading previo;
el encabezado ahora es Custom) y
`disconnected_provider_shows_one_connection_action_instead_of_models` (rutas con
credencial ausente ya no producen la acción esperada). Revisar esos contratos
y repetir workspace después de la edición visual. Clippy compiló sin errores
con 47 advertencias, incluida too_many_arguments del tooltip nuevo.
Esta observación no es aceptación completa del catálogo/UI.

Actualización de integración: la nueva corrida workspace sólo falla en
`disconnected_provider_shows_one_connection_action_instead_of_models`; el test
del encabezado de cuenta pasó (suite app: 514 passed, 1 failed, 2 ignored).
Clippy terminó sin errores, con 47 advertencias. Después de fmt, el check
detectó nuevas ediciones sin formato en settings_ui/providers.rs. Revalidar
esos contratos y formato sobre el worktree al terminar la edición visual.

Verificación final de formato de este bloque: después de aplicar fmt de nuevo,
`cargo fmt --all -- --check` y `git diff --check` pasaron sobre el worktree
observado. No se repitió la suite completa sólo por cambios de formato.

## Reparto actual para continuar en otro chat

El agente del harness mantiene motor del catálogo, registro abierto, consultas
directas, capacidades por ID exacto, presupuesto, transporte y recuperación.
El trabajo visual puede continuar sobre Settings, composer y openide-ui con
los datos y acciones conectados. No sustituir capacidades desconocidas por
soporte supuesto ni confundir listado verificado con inferencia verificada.
Para cambios en model_catalog.rs, native_provider.rs o backend.rs, coordinar
primero el bloque compartido; app.rs y view.rs también tienen cambios activos.
El prompt del inicio de este documento sirve como instrucción para ese chat.

La última prueba enfocada del harness amplió
`plan_build_uncertain_effect_reopens_resumes_and_replays_checkpoint`: pasó con
HTTP local y verifica objetivo/criterios completos en Plan y Build, snapshot
Active en Resume aunque el ledger fue pausado, compactación/checkpoint y
reapertura con un siguiente Plan explícito que recibe el goal Paused completo.
El efecto de archivo ocurre una sola vez. No es aceptación externa, muerte
real de proceso en este caso, ejecución autónoma del goal ni cierre de criterios.

Verificación posterior: la aceptación de cortes de `native_http_cycle` pasó
con 21 interrupciones reales (13 básicas y 8 de hooks en Unix). Incluye
SystemInstructionsCommitted y ModelContextCommitted; el servidor comprueba el
objetivo y criterio completos en cada petición de inferencia, y tras checkpoint
y reapertura se verifican dos snapshots exactos Plan/Build, ledger revision 0
y ningún criterio completado artificialmente. El efecto sigue siendo único.
No instala goal tools en este fixture ni demuestra el ciclo autónomo o UI integral.
La corrida workspace vuelve a fallar en
`disconnected_provider_shows_one_connection_action_instead_of_models`
(514 passed, 1 failed, 2 ignored de app); el trabajo visual sigue concurrente.

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

El harness añadió presentación localizada de recibos en la tarjeta de verificación
de herramientas: `ToolVerificationView.goal_receipt`, un update tipado de ResumeWorker
y un scroll con estado/IDs/revisiones. No cambia catálogo ni acciones de proveedores.
El gate sigue requiriendo texto manual, sin prellenado ni confirmación automática.
Las dos pruebas nuevas pasaron; modificar esa tarjeta debe preservar el gate y
los datos completos de recibo/argumentos/hooks. Se eliminó la duplicación concurrente
de description/input_modalities en dos fixtures de DiscoveredCodexModel.

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

El harness amplió el dispatcher de startup para reservas Ready sin admisión, con
reconstrucción del spawn desde el journal padre y verificación del digest original.
Los bloques localizados son child_worker_request/startup candidates de app.rs,
SubagentDispatchHandle y preflight/readback en openide-harness. No cambia modelos
ni acciones del catálogo. Mantener el diagnóstico/retry de despacho existente.

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

El harness agrega un informe global completo de mantenimiento de goals en el
editor: NativeGoalUiState.recovery, proyección preservada al cambiar sesión,
scroll native_goal_recovery_details y acción native_goal_maintenance_retry.
El retry llama recover_idle_sessions bajo los leases propios y refresca warnings;
se bloquea durante ejecución y fuera del editor. App/composer tienen pruebas
nuevas/ampliadas preparadas. La primera compilación observada quedó detenida
por TooltipSection::IconRow nuevo sin brazo en el match de tree.rs: revalidar
cuando se complete esa edición visual, sin reemplazar el renderer por un placeholder.

La edición visual completó IconRow y los cinco iconos nuevos del host/exporter.
La selección `cargo test -p agents-window goal` pasó 15 pruebas, incluidas
mantenimiento real/reopen y presentación del informe íntegro/retry a 420/900 px.
No se agregaron placeholders al renderer ni se cambiaron acciones de proveedores.
El informe global se conserva al cambiar selección, y el retry se valida también
en el host fuera del editor/durante ejecución.
