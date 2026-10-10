# Automatizaciones nativas: origen y autoridad de lectura

Contrato implementado en Linux el 2026-09-30. El perfil actual es
`ReadOnlyV1`; esta etapa no habilita efectos por heredar permisos globales.
El roadmap conserva los criterios de automatizaciones pendientes.

## Origen durable

`Session.native_automation` contiene un `NativeAutomationOrigin` versionado:
automatización, ocurrencia, momento programado, indicador manual explícito,
perfil, snapshot de autoridad, digest, primer turno y digest del pedido original.
Se publica junto con la metadata antes de admitir el turno o arrancar el worker.
Una sesión nativa interactiva o CLI anterior conserva `None`; no se adopta como
automatización ni se le atribuye un origen retroactivo.

`SessionStore::create_native_automation` deriva un ID de sesión del workspace
canónico y la ocurrencia. La operación se serializa con el lock del journal:
dos handles producen la misma sesión; un snapshot diferente entra en conflicto.
La ocurrencia eliminada conserva el tombstone de generación y no se recrea.
Renombrar, fijar o archivar la sesión conserva su origen.

Los IDs de automatización y de ejecución manual son UUID v4 canónicos. Una
ocurrencia programada usa exactamente `automation_id:scheduled_at_ms`; la
manual usa `automation_id:manual:uuid`. El valor `manual` no se infiere del
texto en el host. Definiciones antiguas editadas con IDs no canónicos no reciben
un origen nuevo válido de forma implícita.

El primer y único pedido admitido debe coincidir en ID, texto, modelo y Build.
No acepta imágenes, nuevas admisiones, steering o subagentes. Reabrir valida
también el journal y bloquea una admisión distinta, duplicada o ausente en un
journal no vacío. Las rutas de append de compatibilidad no eluden la admisión.

## Snapshot del host

`FrozenNativeAutomationAuthorityV1` valida un esquema estricto antes de leer
credenciales. Guarda workspace canónico, proveedor e ID exactos, protocolo,
endpoint, nombre de variable de credencial, capacidades, campos de solicitud,
tope de salida, máximo declarado, configuración de búsqueda alojada, presupuesto,
límites, instrucciones completas y prompt. También fija las huellas del backend,
catálogo, política y hooks vacíos. No guarda el valor de la clave API.

El catálogo contiene `read_file`, `list_files`, `search_files` y `update_plan`.
Los límites V1 son 24 pasos de modelo, 64 llamadas y 16000 caracteres de salida
por herramienta. No tiene herramientas de escritura, comandos, goals, MCP o
subagentes. La configuración de búsqueda del proveedor, cuando corresponde,
se conserva como parte del perfil exacto de petición original.

El snapshot JSON completo está limitado a 512 KiB; incluye escapes y todas sus
propiedades, no sólo las instrucciones. El prompt tiene un máximo de 64 KiB.
La metadata de sesión admite hasta 4 MiB. Un exceso falla antes de admitir;
no se recorta. En Unix, los temporales JSON se crean con permisos `0600`.
Los checksums detectan inconsistencias; no son autenticación frente a un usuario
local que pueda reemplazar todos los archivos y sus digests.

## Despacho y recuperación

El host congela y valida la autoridad antes de crear la sesión. Luego guarda
el enlace del ledger, admite el primer turno y arranca el runner con un lease.
Un retry de la misma ocurrencia con turno ya admitido devuelve la misma sesión
sin otro worker, admisión o petición. Las instrucciones/configuraciones actuales
no sustituyen el origen de una ocurrencia ya registrada.

El ledger del scheduler sigue acotado a 256 entradas. Una sesión con origen
conserva su autoridad aunque se recorte o se edite la definición. Adjuntar una
sesión existente no crea un claim, no vuelve a despachar y no transforma un
Pending/Interrupted en éxito. Un error después del enlace conserva el ID de
sesión. La confirmación de despacho no certifica el resultado final del modelo.

Una sesión publicada sin admisión ofrece **Iniciar ejecución registrada**.
El inicio explícito relee la metadata bajo lease y admite ese mismo pedido.
Un turno interrumpido usa `ResumeWorkerRequest.native_automation`: debe ser
idéntico al DTO reconstruido de la sesión. El worker valida la ruta, backend,
instrucciones, catálogo, presupuesto, política y límites antes de preparar
servicios o pedir al modelo. No instala wrappers de goals/subagentes, MCP,
hooks, actor de comandos ni grants de los ajustes interactivos.

La verificación manual de una herramienta incierta conserva el gate humano y
usa el límite original. Descartar un lote no autoriza un pedido nuevo en esa
sesión. Compactar una automatización terminada usa su modelo, presupuesto y
compatibilidad originales; no cambia su autoridad ni permite otro turno.

Las APIs de intenciones manuales, registro de captura, actor y registro de
comandos también rechazan este origen. La ausencia de un botón de envío no es
el mecanismo que impide ampliar los permisos.

ReadOnlyV1 usa exclusivamente sus instrucciones combinadas congeladas: no
descubre ni activa otra jerarquía de AGENTS durante una lectura. Un marcador
de activación de instrucciones agregado a ese turno se rechaza. Los turnos
interactivos conservan el descubrimiento jerárquico documentado en
[project-instructions.md](project-instructions.md).

Antes de ejecutar, el destino canónico actual debe ser el workspace registrado.
Un directorio renombrado y reemplazado por un symlink a otro proyecto se
rechaza antes de admitir o continuar. El historial y la proyección del snapshot
siguen consultables sin exigir que el directorio exista. Esta comprobación no
es un descriptor de directorio mantenido durante todas las syscalls ni una
garantía frente a reemplazos concurrentes después de la validación.

## Presentación

La sesión muestra origen, ruta registrada y perfil de lectura. Conserva
detener una ejecución activa, recuperación y **Nueva tarea nativa**; no presenta
composer de mensajes ni steering. Memoria deshabilita la edición de goals y
mantiene las consultas de contexto y copias históricas.

Contexto usa la ruta y límites del snapshot sin consultar credenciales o
proveedores actuales. El uso sólo se atribuye a respuestas con esa misma
procedencia. Presenta bytes/cantidades sin convertirlos en tokens por categoría.
Antes de admitir, las instrucciones registradas no se anuncian como enviadas.

## Resultado del turno y ledger de despacho

`AutomationRun.status` conserva el resultado del despacho. `Dispatched` no
significa que terminó el agente. `native_result` versión 1 guarda por separado
Completed, Failed, Cancelled o Interrupted, la necesidad de recuperación,
origen/ocurrencia/workspace, primer turno, digest de autoridad, secuencia/digest
del terminal y secuencia/digest del prefijo completo observado. Los digests
usan SHA-256 y serialización canónica con campos delimitados; no autentican
modificaciones hechas por alguien con acceso de escritura a los stores.

La reconciliación obtiene un lease libre y vuelve a cargar metadata/journal.
Un worker ocupado no se declara interrumpido. Una metadata sin admisión conserva
el enlace y no inventa un resultado de ejecución. La derivación valida el DTO
original, única admisión, catálogo/política/hooks/instrucciones/contexto/ruta y
replay completo con checkpoints de la compatibilidad original, sin credenciales,
Settings, red ni ejecución de herramientas. El cálculo histórico de contexto
usa el workspace registrado sin resolver un symlink actual; no autoriza ejecutar
en una ruta que ya no existe o cambió.

El ledger recibe un token privado de derivación sin Deserialize. El DTO
serializable sirve como estado guardado, y no admite una nueva observación por
sí solo. Rechaza scopes ajenos, claims recortados, secuencias antiguas, cambios
de prefijo y reemplazos de terminal. Una extensión válida puede cerrar la
recuperación de Failed/Cancelled después de verificar un resultado pendiente;
no los transforma en Completed. Un segundo terminal en la misma admisión se
rechaza. Guardar sincroniza archivo, rename y directorio padre en Unix antes
del ACK; el resultado es una proyección reconstruible del journal, no una
transacción atómica entre los dos stores.

App reconcilia al abrir, finalizar worker/Resume, compactar, resolver llamadas,
reintentar un despacho y en el tick del scheduler de 30 segundos. Si la
desconexión de un canal precede a liberar el lease, esa consulta se difiere
hasta una comprobación posterior. El recorrido es síncrono y limitado por las
256 entradas retenidas de cada ledger; no se certifica latencia con journals
grandes ni múltiples escritores desktop.

La consulta usa `SessionStore::load_strict`: una cola truncada o sin delimitador
produce diagnóstico y conserva sus bytes. La lectura normal y la recuperación
explícita mantienen su contrato previo de reparación; esta aceptación no
afirma que toda consulta histórica de la app use el lector estricto. Un journal,
checkpoint o prefijo inválido conserva el último resultado guardado con aviso
actual separado. Los avisos se vinculan a workspace y run ID, por lo que dos
proyectos con una definición copiada no se pisan. Historial presenta resultado,
despacho, recuperación requerida y Abrir sesión desde la UI propia.

La validación de borradores comprueba forma y límites de proveedor/modelo
guardados, sin resolver contra una lista de proveedores incluidos ni leer claves.
El registro local y el perfil exacto se validan antes de admitir al despachar.

## Evidencia y límites

Las pruebas core verifican creación entre handles, reopen, corrupción sin
reparación, límites, una sola admisión, rechazo de steering/child, tombstone,
permisos Unix y tres cortes de proceso en publicación de metadata: temporal
sincronizado, rename y directorio sincronizado. El helper sale sin destructores;
no simula una pérdida de energía ni un corte dentro de una syscall.

Los tests del DTO ejecutan HTTP local Chat y Responses después de modificar o
corromper los archivos actuales, preservando capacidades, tope, presupuesto y
búsqueda alojada. El fixture de credenciales verifica que el secreto no se
serializa y que un DTO inválido falla antes del lookup. No es una cuenta real.

Las pruebas del ledger distinguen enlace y despacho, fallos de publicación,
recorte y reapertura. UI verifica controles, foco, pointer y alcance de selección
a 360/900 px. La prohibición de comandos tiene pruebas core y harness separadas.

`frozen_readonly_automation_resumes_after_real_process_cuts` termina diez
subprocesos sin destructores después de admisión, catálogo, política,
instrucciones, contexto, ruta, comienzo/commit de modelo y comienzo/resultado de
lectura. Reabre sesiones y checkpoints y recupera mediante el worker desktop:
un solo pedido admitido, dos solicitudes SSE en total, una llamada `read_file`
y origen/prefijo intactos. El corte de ToolStarted espera una lectura verificada
manualmente. El helper usa un journal delegado y valida su admisión explícitamente;
no es un proceso de ventana GUI ni mantiene el builder de steering. Una clave
de fixture se inyecta sólo por un seam `cfg(test)`, sin mutar el entorno global
ni cambiar el snapshot producido por freeze. Catorce mezclas de autoridad se
rechazan sin preparación, transporte o append.

Dos aceptaciones App ejecutan lectura HTTP/SSE con política interactiva más
amplia instalada, pero hooks/MCP sin efectos y ningún manager de comandos.
Un retry después de corromper los ajustes conserva despacho, journal y número
de requests. La otra parte de metadata sin admisión, pulsa la recuperación real,
preserva ruta/instrucciones/cap original con archivos actuales dañados y termina
el turno. La compactación manual usa una tercera solicitud sin herramientas,
confirma el checkpoint y lo reabre comprobando su compatibilidad.

`automation_rejects_replaced_workspace_before_admission_and_worker_effects_but_keeps_history_readable`
comprueba en Unix metadata vacía y admisión sin inicio, un symlink real a otro
proyecto, y App/worker con modelo ya construido. Conserva metadata, historial y
origen consultables sin una conexión HTTP o evento nuevo. Las acciones retrasadas
de guardar/quitar la meta tampoco mutan el archivo legacy o el mapa en RAM.

El tramo de resultado agrega seis pruebas App de HTTP/SSE local, Failed y
Cancelled reales, dos cortes de proceso después de delta/TurnCompleted,
reapertura y Resume sin relanzar la ocurrencia, checkpoint semántico, evidencia
corrupta sin reparación y separación de proyectos con el mismo run ID. Cinco
pruebas ledger y dos UI verifican publicación/idempotencia/prefijo/terminal,
scope/recorte y presentación a 360/900 px. Dos pruebas del derivador usan
prefijos fixture y la API real de verificación de lectura incierta: cierran
recuperación conservando Failed/Cancelled, sin inventar Completed. Un reader
core estricto y el digest histórico puro tienen pruebas propias. Esta evidencia
es distinta de los cortes de origen/runner y no certifica cuentas reales.

Continúan pendientes permisos configurables por automatización, aprobaciones
con alcance run/sesión/request, ejecución de efectos y su recuperación,
múltiples procesos desktop del scheduler, aceptación de cuentas/proveedores
reales y plataformas distintas de Linux.
Este contrato no certifica Windows, macOS, power loss o paridad funcional completa.
