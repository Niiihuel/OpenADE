# Streaming nativo y recuperación

## Chat integrado en el dock (7 de octubre de 2026)

IDE y Agentes comparten la misma composición nativa. Los mensajes del agente
usan el ancho de lectura disponible, con márgenes de 12 px en el dock estrecho
y 24 px en la vista amplia. Las filas de actividad y razonamiento miden 28 px,
con iconos de 14 px y controles de 24 px; las tarjetas de edición conservan un
encabezado de 32 px. La tipografía del contenido no cambia.

El último bloque de razonamiento recibido se despliega mientras llega y se
contrae al comenzar texto o una herramienta. Abrirlo o cerrarlo manualmente
tiene prioridad sobre ese comportamiento automático. El cuerpo conserva su
scroll independiente, con un máximo de 160 px. Sólo el bloque activo muestra
progreso; no se agrega un segundo indicador «Pensando» debajo. La animación
respeta movimiento reducido y no modifica los datos del journal.

Los comandos reúnen encabezado, directorio de trabajo cuando existe y salida
capturada en una sola tarjeta. Los permisos siguen usando el controlador de
aprobación existente: una entrada preparada o confirmada no se presenta como
ejecutada. Los errores y resultados inciertos permanecen visibles con la
tarjeta cerrada. El descarte explícito de un lote marca únicamente las llamadas
restantes como «Descartada · sin ejecutar». Al reanudar, una herramienta o un
permiso reutiliza su fila original; el aviso de efectos inciertos requiere un
`ToolStarted` sin resultado confirmado.

El seguimiento del hijo seleccionado conserva el final también cuando llegan
salidas de herramientas, permisos o el cierre sin texto adicional. Si el lector
sube por el historial, se mantiene su posición. Ningún evento de otra sesión
hereda el seguimiento de la conversación visible.

Referencia de comportamiento: [Agent Panel de Zed](https://zed.dev/docs/ai/agent-panel)
y la copia local `references/zed/crates/agent_ui/src/conversation_view/thread_view.rs`
(`render_tool_call`, terminales y composición del turno), junto con
`entry_view_state.rs` (expansión automática y elecciones del lector).
La implementación usa `openide-ui` y las proyecciones del harness de OpenIDE.
Las fixtures `chat_dock_visual_tests` recorren eventos locales reales de la
proyección; `export_chat_dock_visual_review` genera capturas con las fuentes
nativas en `/tmp/openade-chat-dock-review`, sin invocar proveedores ni herramientas.

## Adaptación del chat de OpenIDE a Rust (2 de octubre de 2026)

La presentación utiliza `openide-ui::ToolActivity` y el catálogo semántico de
`tool_presentation.rs`. Las filas muestran la operación, el destino y el estado
real para archivos, búsquedas, cambios, comandos, web/navegador, MCP, subagentes,
planes, objetivos, skills y reglas. Los nombres desconocidos siguen visibles.
Los argumentos provisionales sólo aportan campos completos del objeto JSON
superior; no habilitan ejecución. La entrada de terminal y el contenido escrito
no se usan como título.

Errores, permisos pendientes y resultados inciertos permanecen visibles. Sólo
las lecturas y búsquedas completadas correctamente se agrupan como exploración;
una herramienta abierta conserva su detalle visible. El encabezado despliega
el cuerpo dentro del chat: abierta, la fila y su cuerpo son un solo bloque con
relleno que se abre y se cierra en su lugar, como `Collapsible` de limn.
«Inspeccionar actividad» abre el panel lateral.
Cada elección pertenece a la conversación y sobrevive a la llegada tardía del
`call_id`, a la confirmación del modelo y al comienzo de ejecución, incluso si
esas transiciones llegan en el mismo lote. Los diffs siguen mostrando cambios
confirmados del historial de archivos.

Las acciones «Abrir archivo» y «Abrir enlace» se resuelven desde los argumentos
confirmados de la llamada, dentro de la conversación seleccionada. Los archivos
deben existir dentro del workspace canonicalizado; los enlaces admiten HTTP(S).
Un preview de argumentos no habilita navegación ni ejecución.

Las imágenes MCP se conservan en `ToolResultMedia`, antes del recorte textual
de `ToolFinished`. Este evento observa el resultado de una llamada MCP iniciada
y no confirma por sí solo el efecto. Las imágenes que marca para el modelo
(`model_images`) se suman al resultado que confirma `ToolFinished`; ver
[Imágenes de resultados de herramientas](native-tool-images.md).
La proyección convierte sus datos una vez a `RasterImage` compartidas y las
presenta en la misma tarjeta, incluso al reabrir el journal. Así una captura
normal no depende de caber como base64 dentro de los 16.000 caracteres del
resultado textual. Se admiten hasta cuatro imágenes y 8 MiB agregados por llamada,
con texto de hasta 4 KiB y avisos acotados. Se verifican base64, firma MIME y hash;
el renderer aplica además sus límites de píxeles y decodificación. Los formatos
no admitidos o el contenido omitido se indican sin reconstruir datos truncados.
Las capturas se abren ampliadas en el visor nativo y usan sus controles de galería,
sin volver a descargarlas ni escribir archivos temporales. La selección se
valida por conversación, llamada y digest; cambiar de conversación cierra un
visor que ya no pertenece a la vista actual.

La terminal usa un encabezado de una sola línea: ejecutable u operación, icono y,
mientras la llamada está activa, un indicador de ejecución. El comando completo
se muestra en el cuerpo y en el tooltip del encabezado. El directorio de trabajo se
consulta en el tooltip o el inspector; no se repiten etiquetas de éxito, cwd ni
el JSON de entrada en la tarjeta. El inspector tiene un control independiente
del encabezado que expande la salida. La vista compacta presenta hasta seis
líneas; las líneas largas se recortan visualmente y se pueden leer al expandir.
El cuerpo expandido conserva su scroll al recibir más salida y tiene una altura
máxima de 240 px en vistas compactas y 320 px en las amplias.
Los resultados de comandos
persistentes se interpretan sólo cuando corresponden a su esquema nativo;
omisiones, bytes no textuales y errores permanecen identificables. El cuerpo
expandido tiene límites de presentación y acceso al inspector completo.
`start_command` confirma el inicio; no afirma que el proceso haya terminado.

`ToolOutputDelta` agrega observaciones incrementales de stdout/stderr con el ID
de la llamada. El runner y Resume publican cada observación después de guardarla.
La proyección inserta un preview inmediatamente después de su herramienta y
retiene una cola móvil de como máximo 32 KiB, sobre límites UTF-8. La UI la compone
dentro de la tarjeta, sin otra fila. `ToolFinished` aporta el resultado
autoritativo y conserva por separado la salida capturada: el límite del resultado
final no elimina diagnósticos ya recibidos. Una cancelación o interrupción conserva
la salida parcial sin confirmar éxito ni habilitar reintentos. Los deltas no ingresan al historial
enviado al modelo. El canal tiene 16 lugares, fragmentos de hasta 4 KiB y un
presupuesto de 128 KiB/256 eventos por llamada, con indicación de truncamiento.
El seguimiento del scroll interior conserva las últimas líneas cuando el usuario
estaba al final y respeta su desplazamiento manual. Una resolución verificada
después de cancelar actualiza la tarjeta original y conserva su salida capturada.

La captura incremental de `run_command` está implementada en procesos host y en
el sandbox Linux. El protocolo del helper de elevación y el sandbox Windows MXC
siguen entregando el resultado al terminar; no se presentan como validados con
streaming incremental por esta adaptación.

El Markdown vivo interpreta provisionalmente el último bloque incompleto:
énfasis, código y etiquetas de enlaces se muestran mientras sus delimitadores
llegan. La respuesta final se interpreta literalmente con su contenido completo.
Los fences de código respetan carácter y longitud de apertura, incluidos `~~~`
y backticks largos. El caché acotado reutiliza los bloques estables y vuelve a
interpretar inline sólo donde cambió contenido. La revelación visual avanza por
grafemas completos (acentos combinados, emojis, banderas y secuencias ZWJ).

`TranscriptRows` registra accesos mutables y desplazamientos de índices. El host
publica únicamente las filas modificadas y el sufijo afectado por inserciones o
eliminaciones. El historial anterior conserva sus strings e imágenes durante
las ráfagas; una nueva proyección establece un snapshot completo. El presupuesto
de 64 eventos por frame y el seguimiento manual del scroll siguen vigentes.

Los adaptadores Chat Completions, Responses y Anthropic abandonan conexión,
esperas y reintentos cuando se cierra su consumidor. Los fallbacks JSON publican
argumentos antes de `ToolCallReady`; Responses completa previews que sólo llegan
en el snapshot final sin repetir los deltas ya emitidos.

Pruebas específicas: `native_stream_lifecycle`, `tool_output`,
`tool_media`, `transcript::stream_tests`, `tool_disclosure_app_tests`,
`tool_navigation_app::tests`, `tool_result_media::tests`, `rich_text::tests`,
`stream_presentation::tests`, `tool_presentation::tests` y
`session_surface::stream_visual_tests`. Las fixtures usan servidores/procesos
locales y cuadros `UiTree`; no requieren credenciales. Las referencias concretas
están registradas en [references.md](references.md).

La regresión del 2 de octubre pasó con **2.019 pruebas aprobadas, 66 ignoradas,
26 suites y cero fallos** (`cargo test --workspace -- --test-threads=2`), en
85,51 s de ejecución de tests. La exportación visual se ejecutó por separado y
se revisaron tarjetas, salida en curso, imágenes y visor a 360 y 900 px. Las
capturas de referencia están en `target/visual-qa/streaming/` (artefactos locales,
no fuentes versionadas). Estas pruebas no certifican proveedores externos,
privilegios reales ni el renderer de una ventana del sistema operativo.

Clippy estricto de `openide-core`, `openide-harness` y `openide-ui` pasó sin
advertencias. El desktop pasó Clippy y conserva las 71 advertencias previas;
las advertencias introducidas durante esta adaptación fueron corregidas.
`git diff --check` pasó. Tras la limpieza se repitieron las siete pruebas del
catálogo y los previews de herramientas, todas aprobadas.
El build final `cargo build --locked -p agents-window` pasó sin errores ni
advertencias; el binario actualizado está en `target/debug/agents-window`.

## Ejecución normal y frames del desktop

El adaptador HTTP entrega eventos tipados al runner; éste agrega `AssistantDelta`
para el texto y `ModelStreamUpdate` para los demás datos al journal antes de
publicarlos al host. `AgentRunner::model_step` conserva la cola
ya aceptada también si el backend falla o se cancela en la misma poll: descarta
su future, cierra el receptor y drena ese prefijo antes del evento terminal. No
espera a productores retenidos ni confirma una respuesta parcial. Los tests
`native_streaming_tail_*` verifican éxito, fallo y cancelación, reapertura real y
el historial del siguiente pedido al modelo.

El desktop extrae hasta 64 registros por frame para runs normales, automatizaciones
y updates de Resume y subagentes. La cola no leída conserva FIFO; la desconexión sólo se
procesa después del último registro, incluso cuando el productor ya terminó.
Este presupuesto de lectura deja pasar pintura y acciones entre ráfagas. Los
canales de producción ahora tienen capacidad fija, descrita abajo; el presupuesto
por frame por sí solo no limita el costo de un registro individual. La app solicita repaint
mientras el worker está activo; esto no constituye una garantía de latencia.

`native_streaming_queue_tests.rs` comprueba un burst de 512 deltas con Unicode,
texto visible en la proyección intermedia, terminal detrás del tail, entrega
exacta y equivalencia con replay; también distingue cola viva vacía de cierre
en el límite exacto del presupuesto. Es evidencia de entrega y proyección, no
de transporte HTTP.

Las pruebas de `native_streaming_app_tests.rs` usan TCP/HTTP/SSE
local y el runner, store, polling y árbol UI reales:

- Primer y segundo frame de texto visibles antes de liberar el terminal del
  servidor. Los chunks HTTP dividen UTF-8; commit y reopen producen una sola
  respuesta completa y un solo historial autoritativo.
- Cancelar desde el botón real después del primer frame cierra el transporte,
  conserva el prefijo y no crea otro pedido, herramientas ni respuesta confirmada.
- Un burst de 96 deltas detrás del primer frame verifica que un poll no adelanta
  el cierre: se mantienen ejecución y texto parcial hasta consumir el tail.
- El scroll sigue el final cuando el usuario estaba allí, conserva la posición
  después de un desplazamiento manual y ofrece volver al final.
- EOF sin terminal SSE y cuerpo HTTP cortado abruptamente muestran error y
  conservan el texto parcial al reabrir, sin commit, historial parcial ni retry.

El fixture retiene explícitamente cada frontera antes de permitir otra escritura;
no comprueba el primer frame recién después de recibir toda la respuesta. La
pintura verificada corresponde a `UiTree`/DrawCommands, sin una prueba del renderer
GL de la ventana ni de un proveedor externo. El diseño de transporte incremental
usa como referencia [Streaming Chat de Vercel](https://vercel.com/academy/svelte-on-vercel/streaming-chat),
implementado en el motor y framework Rust propios.

### Plan y controles de sesión dentro del chat

Cada actualización válida del plan aparece en su posición del historial, entre
las respuestas y herramientas que la rodean. `TranscriptItem::PlanSnapshot`
conserva la secuencia del evento, su contenido y si es una propuesta. Un nuevo
`PlanUpdated` o `PlanProposed` agrega otra versión; no reescribe los pasos ni los
estados de una versión anterior. La reproducción del journal conserva ese mismo
orden. No se agrega una tarjeta sintética al final de la conversación desde
`SessionPlanView`.

Los eventos de plan aparecen como una tarjeta **Plan actualizado** o
**Plan propuesto**, con el número de pasos completados alineado a la derecha del
encabezado. Sus pasos permanecen visibles, también durante una ejecución y en
las versiones históricas; no hay flecha ni acción para colapsarlos. Las antiguas
preferencias de expansión no ocultan estos snapshots. Sólo el último snapshot
coincidente con la elegibilidad de `SessionPlanView` ofrece **Construir este
plan**, fuera de una ejecución activa; las versiones anteriores nunca ejecutan
el plan actual. La acción mantiene su revalidación contra el journal. Una llamada
`update_plan` confirmada se representa mediante su snapshot sin repetir una
segunda fila genérica; las llamadas pendientes, fallidas o inciertas conservan
su información de herramienta.

Los pasos, permisos pendientes, recuperación y tarjetas de subagentes comparten
la columna de lectura y el scroll `transcript`. Cada plan mide la altura
real de sus pasos, incluidos los renglones envueltos, sin un segundo scroll ni
una reserva fija por paso. Sus estados usan marcas vectoriales, sin depender de
glifos ausentes en la fuente. **Volver al final** flota dentro del chat y no cambia
la altura del viewport ni la posición del compositor.

El historial no muestra filas ni resúmenes de **Uso de tokens**. La información de
uso se conserva en la proyección y se consulta desde **Contexto**, junto al
compositor. Ocultar esas filas no descarta los eventos ni sus contadores.

Las pruebas de `conversation_plan_visual_tests.rs` verifican versiones
intercaladas antes y después de respuestas, contenido y estado histórico
conservados, ausencia de una tarjeta final adicional y Build sólo en la propuesta
vigente. También comparan la posición del compositor y del viewport, el ancho de
lectura y el acceso al último paso de planes largos. Las capturas nativas cubren
360, 900 y 1700 px, tanto con versiones expandidas como contraídas.

Antes de medir un frame nuevo o de medir de nuevo tras una entrada del usuario,
el host conserva el seguimiento del final cuando el frame previo pertenece a
la misma sesión, su worker sigue activo y el target de scroll estaba al final.
Así el crecimiento del compositor, el tamaño de la ventana y el nuevo plan no
pierden el seguimiento. El target refleja de inmediato wheel/drag, por lo que
una lectura desplazada arriba mantiene su posición. No se hereda esta intención
al cambiar de sesión ni después del terminal. Las pruebas con SSE retenido
comprueban crecimiento, resize, publicación del plan y desplazamiento manual;
también ejercitan la revelación con movimiento normal y límites UTF-8.

## Límites de ensamblado de los adaptadores

### Detección del transporte con cabeceras incompletas

El 1 de octubre se reprodujo con la conexión guardada de Codex una respuesta
HTTP 200 cuyo cuerpo era SSE y no incluía `Content-Type`. Interpretarla como
JSON causaba `invalid Responses JSON: expected value at line 1 column 1` antes
del primer evento. Ahora los tres adaptadores piden `Accept: text/event-stream`
y detectan incrementalmente un prefijo SSE cuando el MIME falta o es incorrecto.
JSON ordinario sigue su propio decoder y no pasa por el framer SSE. El detector
tiene presupuesto acotado y conserva los límites de body/frame.

Declarar o detectar SSE sigue exigiendo frames delimitados y un terminal válido:
EOF, HTML, JSON truncado o una llamada parcial no comprometen la respuesta ni
autorizan herramientas. Las fixtures cubren prefijos fragmentados, BOM,
cabeceras ausentes/erróneas, JSON indentado y esos cierres inválidos. La prueba
Build con el modelo guardado `gpt-5.6-luna` pasó nuevamente en 20,49 s, con commit
durable anterior a `read_file` y Resume Idle sin otro pedido. Esa aceptación
usa un archivo temporal y no prueba operaciones de escritura o administración.

Chat Completions y Responses admiten como máximo **64 llamadas a herramientas
por respuesta del modelo** y **8 MiB agregados de texto, IDs, nombres y
argumentos retenidos**. Se comprueba el límite antes de insertar otra llamada
o ampliar el contenido. Responses comprueba también el tamaño serializado del
output opaco antes de copiarlo al resultado. El JSON final y la respuesta sin
streaming aplican las mismas puertas; los eventos finales que repiten argumentos
ya vistos no cuentan esos bytes dos veces. Una respuesta legítima de 64 llamadas
y un mensaje contiene 65 output items y se admite: items y llamadas son unidades
distintas.

Responses correlaciona los deltas mediante `output_index` e `item_id`, pero
reconcilia los items completos del stream y del terminal por su ID estable.
Los índices del stream pueden tener huecos tras un item agregado vacío; no se
usan como posiciones de `response.completed.output`. `output_item.done`
conserva el objeto original, incluidos reasoning opaco, namespaces y citas;
`response.completed` confirma el cierre exitoso. La colección de items `done`
tiene su propio límite de **8 MiB y 4096 items**; el resultado reconciliado
también debe caber en 8 MiB y contener como máximo 64 llamadas. Se siguen
rechazando IDs duplicados, llamadas sin item completo y diferencias de nombre,
call ID, argumentos o campos compartidos del snapshot. El
[SDK oficial de OpenAI](https://github.com/openai/openai-python/blob/main/src/openai/lib/streaming/responses/_responses.py)
también mantiene un mapa de índices del stream en lugar de asumir posiciones.

Estos límites son independientes de `RunLimits` por turno y del caché Responses
de 24 respuestas. No limitan el RSS total del proceso: permanecen el parsing,
copias temporales y canales externos. Los límites de body y frame no sustituyen
el límite agregado; numerosos frames pequeños y fragmentos de herramientas
vacíos también lo respetan. Los deltas Chat vacíos no se publican ni deshabilitan
el retry previo al primer texto observable.

Las suites `backend::tests` y `responses::tests` verifican estos
casos, incluidos transporte HTTP con retry después de contenido vacío, prefijo
seguido de la llamada 65 sin retry, acumulación al límite sin mutar al fallar,
finales grandes y output opaco. El rechazo no devuelve herramientas ni una
respuesta confirmable. Las pruebas HTTP del runner descritas arriba comprueban
que un fallo del proveedor conserva el prefijo al reabrir sin commit ni efecto.
No equivale a aceptación con un proveedor remoto.

## Referencia de Zed y mejoras del 30 de septiembre

Se inspeccionó el clon `references/zed`, revisión
`d284cbbc87b55d659303106ced627c8612369827`, mediante lectura de código. El
recorrido corresponde al agente nativo: transporte → normalización del
proveedor → thread → proyección de conversación. Su puente ACP dentro del
proceso permite reutilizar la UI; el transporte de agentes externos ACP es
otro recorrido.

### Fallos identificados y corregidos

**Correlación de llamadas.** El error anterior «Responses function call output
index is missing» provenía de tratar `output_index` como posición del array
terminal. La corrección previa usa el ID estable y conserva las validaciones
de identidad y argumentos. Zed mantiene sus llamadas pendientes por `item_id`,
lo que respalda esa separación entre posición e identidad. [Mapper Responses
de Zed](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/open_ai/src/completion.rs#L753).

**Items completos separados del terminal.** El error de la captura
«Responses streamed function call is missing from completed output» se
producía en `ResponseStream::finish`: reteníamos llamadas parciales pero
descartábamos los objetos de `response.output_item.done`. Luego exigíamos
que esas llamadas se repitieran en el array terminal. En Codex, el cierre
normalizado contiene ID y uso; los items completos llegan por separado.
[Evento terminal de Codex](https://github.com/openai/codex/blob/33a0f766a647208b471cfbcad889c67fd324ee04/codex-rs/codex-api/src/sse/responses.rs#L108),
[items completos de Codex](https://github.com/openai/codex/blob/33a0f766a647208b471cfbcad889c67fd324ee04/codex-rs/codex-api/src/sse/responses.rs#L353).

Ahora se retienen los items originales de `output_item.done` y se
reconcilian con un snapshot terminal completo, parcial, vacío o sin `output`.
Un `output` presente que no sea array, incluido `null`, sigue siendo inválido.
La reconciliación compara campos compartidos y conserva campos omitidos;
no reconstruye llamadas ejecutables desde deltas ni desde
`function_call_arguments.done` aislado. Si un snapshot mezcla items nuevos
con la omisión de items `done`, no se puede deducir su orden completo y se
rechaza explícitamente. Así no se recoloca reasoning antes de otra llamada.
Un snapshot con todos los items conocidos conserva su orden final; uno que
sólo contiene parte de los conocidos se completa con el orden del stream.

Zed correlaciona la llamada y la completa al recibir argumentos finalizados;
también conserva los items opacos de reasoning. Adoptamos la frontera
conservadora de item completo, terminal válido, JSON y catálogo validados,
y commit durable antes de ejecutar herramientas.
[Normalización de Zed](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/open_ai/src/completion.rs#L916),
[reasoning de Zed](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/open_ai/src/completion.rs#L1056),
[eventos oficiales de Responses](https://developers.openai.com/api/docs/guides/streaming-responses).
La incompatibilidad del parser se comprobó por lectura de estos contratos y
fixtures HTTP; la forma exacta del tráfico que causó la captura sigue siendo
una inferencia. No se capturaron conversaciones privadas. La aceptación real
con la conexión guardada de Codex se documenta más abajo.

**Plazo de lectura.** Los tres adaptadores usaban un timeout total de 120 s.
Eso podía cortar una respuesta sana de más de dos minutos aunque continuara
llegando texto. Ahora el builder compartido de `backend.rs` limita conexión a
30 s y cada espera de lectura a 120 s, sin plazo total predeterminado. La espera
se reinicia después de una lectura exitosa; los heartbeats SSE también cuentan
como actividad. Un razonamiento que no envía ningún dato durante más de 120 s
todavía puede agotar esa espera. `with_timeout` conserva su comportamiento de
límite total explícito. Esta corrección aplica a Chat Completions, Responses y
Anthropic; Anthropic conserva su política de no seguir redirects. [Semántica
de reqwest 0.12.28](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html#method.read_timeout).

**Errores Responses.** Se descartaba `error.code`; `error` anidado no aportaba
su mensaje y `response.error` se ignoraba. Ahora se reconocen `response.failed`,
`response.incomplete`, `response.error` y `error`, leyendo los campos planos y
los envelopes `error` o `response.error`. Se conserva código y mensaje en el
diagnóstico, acotados a 256 y 1.000 caracteres. La conversación diferencia
`subscription_sharing_usage_limit_exceeded` de
`subscription_sharing_usage_unavailable` aunque no aparezcan en el mensaje:
el primero informa un límite de uso en OpenIDE y dirige a ChatGPT → Configuración
→ Uso mediante «Ver uso» del proveedor; el segundo informa que no se pudo comprobar
la disponibilidad y propone reintentar más tarde. El código de límite no demuestra
que todo el plan esté agotado ni determina cuándo se restablece: también puede
aplicar un límite de la app. Ambos conservan el diagnóstico original en el journal
y muestran un único aviso por turno, también al reabrir.
También se aplica al resultado JSON fallido. Sigue siendo un error terminal;
el resultado parcial no autoriza herramientas. [Formatos admitidos por
Zed](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/open_ai/src/responses.rs#L432),
[errores de uso del plan documentados por
OpenAI](https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery).

**Textos de rechazo.** Responses sólo publicaba `output_text.delta`, pero el
final incluía también `refusal`: el rechazo puro se mostraba al terminar y un
resultado mixto podía fallar la reconciliación. `response.refusal.delta` ahora
usa la misma entrega y límites que el texto. Chat Completions incorpora
`delta.refusal` y `message.refusal`; un rechazo con `content:null` ya no queda
vacío. Publicar estos fragmentos también impide reintentar transparentemente
después de que el usuario recibió texto. [Esquema oficial de los eventos de
rechazo](https://platform.openai.com/docs/api-reference/responses-streaming/response/refusal/delta).

**Partes del resumen público.** Responses enviaba `summary_index`, pero el
adaptador ignoraba esa frontera y pegaba párrafos distintos del mismo item.
Ahora inserta una separación de párrafos cuando cambia el índice con texto,
conservando el `item_id` original. Los deltas vacíos no crean separadores; otro
item mantiene su propia identidad. Un gateway sin índice conserva su formato
de resumen único. El separador cuenta dentro del presupuesto agregado de 8 MiB
y una ampliación rechazada no modifica el estado ni publica un fragmento parcial.
Se recuerda el índice por item, incluso si sus fragmentos se intercalan con otro;
el mapa admite como máximo 4096 items y cada ID retenido también cuenta en el
presupuesto. La fixture recorre SSE fragmentado, Unicode, delta vacío, dos partes
y dos items intercalados;
el output opaco y los items originales quedan intactos para replay. Las pruebas
de frontera admiten el tamaño exacto y rechazan un byte adicional. Se revisaron
la [separación semántica de Zed](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/open_ai/src/completion.rs#L1096)
y los [índices y cortes de sección de Codex](https://github.com/openai/codex/blob/33a0f766a647208b471cfbcad889c67fd324ee04/codex-rs/core/src/session/turn.rs#L3011).
Esta normalización es propia y sólo afecta la presentación del resumen público.

Estas correcciones comenzaron con revisión estática y luego recibieron pruebas
unitarias, HTTP, de journal y de interfaz por autorización explícita del usuario.
El error de correlación explica la ruta de código del diagnóstico anterior;
los otros hallazgos no se atribuyen a aquella captura sin un registro del stream.

### Eventos, colas y cache implementados

`ModelBackend::stream_events` es el recorrido común de Chat Completions,
Responses y Anthropic. `ModelStreamEvent` distingue inicio, texto, razonamiento
público, argumentos de herramientas, entrada completa y uso reportado. Cada
registro del journal conserva sesión, secuencia y paso del pedido. Los items de
razonamiento y herramientas tienen identidad estable. El diseño toma como
referencia los [eventos de Zed](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/language_model_core/src/language_model_core.rs#L42)
y su [canal de fondo de 32 eventos](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/language_model/src/language_model.rs#L25),
con implementación propia y contratos de persistencia propios.

- Proveedor → harness: **32 eventos**, cada uno de hasta **32 KiB serializados**.
  El envío espera capacidad y escucha cancelación; fragmenta texto respetando
  UTF-8 y la expansión de escapes JSON. Los collectors viven sólo un frame SSE;
  se publican antes de leer el siguiente. Los buffers HTTP se comprueban antes
  de ampliarse.
- Worker → desktop: **64 registros** en ejecución normal y Resume. Durante el
  streaming, esperar capacidad permite cancelar el transporte. Se descarta el
  future HTTP antes de entregar el registro persistido pendiente y drenar la
  cola aceptada. No se adelanta el terminal ni se descartan registros por falta
  de espacio. La entrega final y liberación del lease todavía esperan que el
  consumidor drene o cierre su receptor.
- La proyección limita la presentación semántica por pedido a **64 filas y
  64 KiB de texto**: razonamiento hasta 16 KiB por segmento y argumentos hasta
  4 KiB. Se diferencian datos parciales, confirmados, cancelados e interrumpidos.
  `ToolCallReady` sólo informa; la validación completa y
  `ModelResponseCommitted` siguen precediendo cualquier efecto. `ToolStarted`
  reemplaza el preview confirmado por identidad de llamada.
- Se muestran sólo summaries/texto público, nunca firmas, reasoning cifrado ni
  bloques redactados. Un error después de texto, razonamiento o preparación de
  herramientas visible no habilita retry transparente. Chat exige argumentos
  finales presentes y JSON válido, sin inventar `{}` para una entrada vacía.
- Markdown terminado tiene LRU de **64 entradas / 4 MiB**; streaming usa
  **2 entradas / 512 KiB compartidos** y la admisión diferida otra de **512 KiB**. Se cuentan
  capacidades retenidas de strings, vectores y boxes. Las claves contienen
  sólo el prefijo visible (20.000 caracteres), truncamiento, ancho, paleta y
  versión del renderer; cambiar sesión o presentación limpia el cache. Sólo
  se almacenan árboles pasivos, sin IDs de acciones compartidos. Hits evitan
  parsing y construcción; el clone de elementos y layout por frame permanecen.

Las APIs legacy de texto siguen disponibles. El default de `stream_events`
adapta backends antiguos mediante su canal ilimitado original y limita el texto
procesado a 8 MiB; un productor legacy que inunda el canal sin ceder no admite
backpressure. Al cancelar se descarta esa cola privada, pero se conservan los
eventos ya aceptados por el canal tipado. **Ninguno de los tres adaptadores de
producción usa ese puente.** Tampoco se promete un límite del RSS total: siguen
existiendo el historial, journal, completions y copias transitorias.

Los tests de esta etapa comprueban las fronteras reales de backpressure:
retener el consumidor impide confirmar el terminal hasta consumir el prefijo
aceptado. Cancelar conserva ese prefijo sin autorizar una respuesta parcial.
Las fixtures distinguen envío HTTP, admisión al canal tipado, persistencia y
pintura; un fragmento enviado por el servidor todavía puede no estar aceptado.

### Presentación visual inspirada en Zed

El chat ahora usa una presentación continua para el Markdown del assistant,
sin repetir un encabezado «AGENTE · ESCRIBIENDO» por bloque. Al aparecer el primer
texto visible se retira el indicador de espera completo; no se agrega una fila
«Respondiendo» ni un spinner debajo de la respuesta. El turno conserva su estado
real y el control Detener hasta recibir el evento terminal. Las filas del pedido
al modelo que ya terminó no duplican la respuesta.

El componente propio `openide_ui::Collapsible` tiene header de 32 px, icono o
spinner en un slot estable, metadata breve y chevron. El host conserva la
expansión por ID y conversación; el componente sólo emite el toggle normal de
puntero/teclado. Razonamiento público, argumentos provisionales y tokens tienen
detalle plegable. Mientras llega razonamiento cerrado se muestra una vista
previa de una línea. El grupo de herramientas se abre durante actividad,
permiso, error o incertidumbre; al terminar se pliega si el usuario no escogió
manualmente otra presentación. Los permisos del composer siguen visibles
independientemente del plegado. Los nombres de archivo, búsqueda o comando
provienen de argumentos estructurados confirmados, nunca de parsear JSON parcial.

`StreamingPresentation` conserva un único cursor de presentación para la fila
activa y ninguna copia de texto. Solicita repintados de **16 ms** y adapta el
avance para alcanzar un burst recibido en unos **200 ms**. Usa límites UTF-8 y
una comprobación defensiva acotada de extremos; el contrato de proyección sigue
siendo append-only. No valida ni transforma el contenido. Al aparecer otra
fila, una herramienta, un terminal o un cambio de sesión se muestra completo
el bloque anterior. Reabrir historia no la anima; reducir movimiento muestra
todo de inmediato. El journal, historial del modelo y autorización de efectos
no esperan a este cursor. El scroll sólo sigue el crecimiento si el usuario
estaba al final; la lectura desplazada conserva su lugar.

Estos patrones se revisaron en el [razonamiento/disclosure de Zed](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/agent_ui/src/entry_view_state.rs#L121),
sus [filas de herramientas](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/agent_ui/src/conversation_view/thread_view.rs#L8726)
y su [revelado de texto](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/acp_thread/src/acp_thread.rs#L4781).
La implementación de componentes y cursores es propia; no se copió código GPL.
Los intervalos son objetivos de presentación, sin mediciones de FPS/latencia
ni inferencia remota en esta etapa. El layout del timeline todavía se reconstruye.

### Decoder SSE y JSON ordinario

Los tres adaptadores comparten `SseFrameBuffer`. Reconoce CR, LF y CRLF,
incluidas combinaciones legales, CRLF dividido entre chunks y BOM UTF-8 inicial.
El cursor no vuelve a escanear bytes ya examinados; la compactación ocurre al
agregar un chunk, no por cada evento del mismo chunk. Se comprueba el presupuesto
de transporte antes de ampliar el buffer. Las pruebas cuentan visitas a bytes
con todos los tamaños de chunk de un stream UTF-8; no son una medición de RSS.

Una línea vacía delimita el evento. EOF no despacha un JSON SSE sin delimitar,
aunque ese JSON esté completo: Responses ya no usa el antiguo flush excepcional
del residual. Un terminal cortado no autoriza herramientas. El fallback para
respuestas JSON ordinarias se mantiene fuera del framer SSE para no interpretar
las líneas vacías de JSON pretty como eventos.
[Contrato de líneas y EOF de SSE](https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream).

### Orden visual, hijos vivos y respuestas extensas

Texto, razonamiento público y previews conservan el orden en que llegan. Un
evento semántico distinto cierra el segmento de texto activo; un delta posterior
crea otro segmento. El commit conserva esos segmentos cuando su concatenación
coincide con el texto autoritativo, y sustituye el provisional si difiere.
Snapshots repetidos y el display posterior no duplican la respuesta. Una llamada
que sólo alcanzó el preview y luego falló muestra **No ejecutada**; el fallo de
una herramienta realmente iniciada conserva su estado propio.

`from_live_events` no inventa una interrupción al cargar un journal cuyo
subagente todavía está ejecutándose. El host consulta el bridge antes del replay
y oculta la acción de recuperación de un hijo vivo. Si la cola de un suscriptor
se llena y pierde el terminal, al reconectar se vuelve a leer el journal:
se recupera el tail aun cuando no llegue otro evento. El polling de hijos también
tiene presupuesto de 64 eventos por frame.

El Markdown visible continúa acotado a 20.000 caracteres y 160 bloques. Cuando
se alcanza un límite, una respuesta terminada ofrece **Ver respuesta completa**.
El modal lee la fuente Markdown original por páginas de hasta 20.000 caracteres
Unicode, con Anterior/Siguiente, scroll y Escape. Cada página usa hasta 40 filas
pasivas de 512 caracteres; no se descarta contenido por el límite de bloques.
Los cursores son offsets UTF-8 exactos y las acciones llevan sesión, fila y página
para rechazar clics obsoletos. El visor espera que termine la proyección viva.

### Verificación autorizada del 30 de septiembre

Se agregaron pruebas unitarias de reconciliación y dos fixtures HTTP con runner,
lector contado y `SessionStore` real. Retener `response.completed` después de
todos los items `done` produce previews, pero ningún commit ni efecto. Liberar un
terminal coherente confirma la respuesta y `ToolStarted` antes de llamar al
lector; la continuación reenvía reasoning opaco y namespace originales. Un
terminal contradictorio, o completo pero sin delimitador SSE, no ejecuta nada.

Las pruebas de transcript cubren segmentación, snapshots distintos/repetidos,
cancelación, reapertura y límites. Dos fixtures de subagentes verifican un hijo
vivo con subscriber lleno y la pérdida del terminal seguida de recuperación sin
otro evento. Las cinco fixtures HTTP del desktop comprueban texto y controles
pintados antes del terminal, cancelación, EOF, backpressure y scroll real del árbol.

La construcción de un host de tests ya no inicia consultas automáticas a
catálogos ni a la cuenta real de GitHub. La prueba aislada de 64 hosts vuelve al
baseline de **4 descriptores / 2 threads** después de cerrar los hosts; antes de
corregir ese arranque, 32 hosts acumularon 308 descriptores / 193 threads. En
producción, `CatalogRefreshJob` cancela los proveedores pendientes y omite la
publicación si observa cancelación antes de escribir el cache. La escritura
que ya comenzó puede terminar. Drop no espera a HTTP; la petición en curso
conserva su límite agregado de 12 segundos.

La cuenta guardada de Codex pasó Plan y Build de sólo lectura mediante la fábrica
de producción. Build confirmó dos respuestas, una lectura, el journal reabierto
y Resume Idle sin otro pedido ni repetición del lector.
Ver [aceptación real y alcance](native-real-provider-acceptance.md#codex-aceptación-real-aprobada-del-streaming-2026-09-30).

Las capturas en `target/streaming-selectors-visual-qa` provienen de los comandos
de pintura del framework propio. Se inspeccionaron permisos compactos, catálogo,
composer multilineal, timeline ancho/estrecho y lectura completa. Son evidencia
de layout, con fuentes SVG del sistema; no equivalen a una comparación pixel a
pixel del renderer WGPU ni a interacción manual con la ventana del SO.

### Mejoras pendientes

1. **Errores tipados y continuidad explícita.** Mantener el code en el texto
   soluciona el aviso actual, pero UI y retry deberían recibir proveedor,
   código, status, categoría y `retry_after` como datos. Zed normaliza esos
   conceptos. Su retry después de salida parcial conserva el mensaje y agrega
   `Message::Resume`: requiere continuidad de conversación y tratamiento de
   efectos. Nuestro retry previo al primer delta sigue siendo apropiado hasta
   diseñar ese contrato. [Clasificación de
   errores](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/language_model_core/src/language_model_core.rs#L195),
   [continuación del
   hilo](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/agent/src/thread.rs#L3114).

2. **Estados de cola.** Para nuevas acciones de cola convienen estados
   explícitos como pausada y esperando el cierre de una interrupción, junto
   con guards de generación. Nuestra cola actual es steering persistido del
   turno activo, diferente de la cola de siguientes turnos de Zed; no se
   encontró un doble envío concreto. [Máquina de estados de
   cola](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/agent_ui/src/conversation_view/message_queue.rs#L24).

3. **Medición por entrada.** El cache evita parsing repetido, pero continúan
   el clone de la proyección y layout completo. Una lista que conserve medidas
   por fila necesita admitir alturas variables de Markdown; no debe sustituirse
   directamente por una lista de filas uniformes.

### Contratos propios que se conservan

El journal antecede la publicación; un fallo de append no se presenta como
guardado. Los deltas ya aceptados se drenan también al cancelar o fallar el
modelo, mientras la persistencia siga disponible. Un fallo del observador de
Resume conserva el contrato de detención descrito abajo.
`ModelResponseCommitted` antecede a los efectos de herramientas y conserva los
items opacos para replay. El desktop mantiene 64 eventos por frame y espera
consumir el tail antes de liberar el worker. No se toma EOF ni `[DONE]` como
equivalente a `response.completed` en Responses. No hay retry transparente
después del texto ni repetición automática de un hosted search aceptado.

Zed agrupa eventos inmediatamente disponibles en una actualización de entidad,
pero ese batch no tiene un límite explícito; su `ThreadEventStream` es
ilimitado. Su persistencia consolida snapshots y `Thread::to_db` toma los
mensajes ya incorporados, no el pending message. Esos patrones no sustituyen
nuestro journal, sus reglas de Resume o la recuperación de efectos inciertos.
[Batch del hilo](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/agent/src/thread.rs#L2952),
[snapshot de mensajes](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/agent/src/thread.rs#L1927).

## Contrato de Resume

`ResumeEngine` consume `ModelBackend::stream_events`. Conserva el texto como
`AssistantDelta` y los demás datos como `ModelStreamUpdate`. Cada registro se
agrega al journal antes de llamar al observador. Streaming usa
`ResumeEventObserver::on_persisted_stream_event` async; las fronteras confirmadas
usan `on_persisted_event`. El observador recibe todos los
eventos nuevos del engine en su orden original, incluidos permisos, resultados de
herramientas y marcadores de checkpoint; no modifica sus contratos de ejecución.

En este contrato, **persisted significa que append tuvo éxito**. No significa un
`fsync` individual por delta. Se conserva la política de sincronización existente
del store y de sus checkpoints. Una pérdida de energía puede perder escrituras
que el sistema operativo todavía no haya sincronizado.

Al completar, fallar o cancelar la llamada al modelo, se completa o descarta su
future, se cierra el receptor y se guardan sus deltas ya aceptados. No se espera a
productores separados después de cerrar el receptor. Luego se propaga el error o
la cancelación. Un prefijo parcial nunca se convierte en
`ModelResponseCommitted`, no entra en el historial enviado al modelo y no ejecuta
llamadas a herramientas incompletas.

Un error del observador detiene la recuperación en el prefijo ya guardado. Un
error de append no llama al observador para ese evento. Ninguno crea una respuesta
final ni un evento terminal ficticio. La recuperación de una llamada al modelo
interrumpida conserva `ModelInFlight` y requiere una nueva acción explícita de
Resume. Abrir un store o pintar su transcript no inicia una petición.

Ese error del observador es distinto de perder el receptor visual del runner.
El runner conserva el tail aceptado aunque se cierre la UI. En Resume, el callback
genérico puede informar un fallo de integridad, secuencia o dueño del lease;
el engine detiene el journal en el primer prefijo observado y no drena el resto
de esa cola. No se ignora el error ni se autorizan efectos posteriores. Distinguir
una entrega visual cerrada mediante un error tipado permitiría conservar ese
tail sin continuar ante errores de integridad; queda como mejora posterior.

La cancelación de Resume mantiene este contrato previo de recuperación manual;
no agrega `TurnCancelled` a una petición de modelo cuya respuesta no se confirmó.
Las reglas de ruta exacta, lease exclusivo, decisiones de permisos, checkpoints
y verificación manual de efectos inciertos siguen vigentes.

## Entrega al frontend

`resume_worker.rs` comparte un cursor entre el observador inmediato, el resultado
final del paso y la lectura del journal después de un fallo. El observador exige
el owner y la secuencia siguiente exactos. Los resultados del paso pueden incluir
un prefijo que ya se entregó; sólo se publica su sufijo nuevo y contiguo. Cada
`ResumeUpdate::Events` contiene un evento, y `Finished` se encola después de los
eventos y la limpieza del worker.

La UI aplica esos eventos a la misma proyección de transcript utilizada para la
reapertura. Los deltas muestran texto incremental durante la petición y la
respuesta confirmada reemplaza ese contenido al terminar, sin repetirlo. Un
prefijo de una petición interrumpida aparece como texto parcial acompañado de la
indicación de interrupción cuando se reconstruye desde el journal.

`ModelResponseCommitted` ya muestra su contenido completo y sus citas sin esperar
el evento redundante `AssistantMessage`. Un corte entre ambos conserva la respuesta
autoritativa al reabrir; el display posterior no la duplica ni la reemplaza. Una
respuesta final pendiente conserva su fila para un `TurnCompleted` agregado por
Resume. Un nuevo turno, request, steer o inicio de herramienta cambia el owner de
la fila activa y evita modificar respuestas históricas. Si el commit sólo contiene
herramientas y texto vacío, se descarta el prefijo de texto no confirmado sin
desplazar los índices de las otras filas.

Esta entrega conserva el presupuesto de eventos por poll que establezca el host.
El cursor evita duplicados; no convierte los canales en una cola con tamaño
máximo ni implementa backpressure hacia el proveedor.

## Evidencia reproducible

En `apps/agents-window/src/resume_streaming_tests.rs`:

- `resume_streaming_http_paints_first_delta_before_terminal_and_delivers_final_events_once`
  usa un servidor TCP/SSE real. Divide un emoji entre escrituras de bytes, bloquea
  el terminal y verifica el primer delta en el journal, la proyección y un frame
  real de `UiTree` antes de liberar el resto de la respuesta. Compara toda la
  entrega con el sufijo exacto del journal y verifica un solo mensaje final.
- `resume_streaming_http_disconnect_preserves_partial_reopen_and_manual_retry`
  y `resume_streaming_http_cancel_preserves_partial_reopen_and_manual_retry`
  verifican el prefijo parcial, la liberación del lease, stores recién abiertos,
  `ModelInFlight`, ausencia del texto parcial en el historial y una nueva petición
  manual sobre la misma ruta. El journal original no se reescribe y no se duplican
  `ModelRouteCommitted` ni `ModelRequestStarted` de esa petición durable.
- `resume_streaming_delivery_rejects_owner_gaps_and_live_duplicates_without_advancing`
  verifica owner, huecos, duplicados inmediatos, replay del prefijo final y fallo
  del receptor sin adelantar el cursor.

En `apps/agents-window/src/transcript.rs`,
`committed_response_cut_reopens_full_content_citations_and_recovers_without_duplicate`
abre stores reales después de cortar exactamente tras el commit, con y sin un
delta incompleto y con y sin herramientas. Contrasta el estado con `replay_native`,
comprueba texto íntegro, orden de citas y Marker completado, y aplica la continuación
sin duplicados ni reescritura del prefijo. Las pruebas de respuesta sólo con
herramientas y de un commit de paso anterior cubren índices de filas y protección
del contenido de una petición más reciente.

`plan_runner_and_resume_commit_cut_preserve_text_and_structured_card_without_duplicates`
comprueba un Plan del runner con streaming y stores reales. Los cortes controlados
tras commit o propuesta preservan respuesta y tarjeta al reabrir; Resume completa
sin duplicados ni otra llamada al modelo. El botón Build aparece sólo después
del terminal, a 360/900 px. Esta prueba no interpreta JSON de plan: el contrato
actual extrae los pasos de la respuesta de texto.

En `crates/openide-harness/src/resume.rs`:

- `recovery_streaming_same_poll_error_preserves_all_accepted_deltas_on_reopen`
  y `recovery_streaming_same_poll_cancel_preserves_all_accepted_deltas_on_reopen`
  publican varios deltas y un error o cancelación dentro de la misma poll del
  backend. Comprueban su persistencia ordenada, su observación posterior al append
  y la reapertura sin una respuesta confirmada.
- `recovery_streaming_observer_failure_stops_at_recoverable_saved_prefix`
  comprueba un error real del callback después del primer append.
- `recovery_streaming_failed_append_never_notifies_or_commits_phantom_output`
  provoca un fallo de escritura del journal después del primer delta y verifica
  que el segundo no se notifica ni se confirma; el prefijo original se reabre.
- `recovery_streaming_cut_after_response_commit_reopens_and_finishes_without_model_retry`
  corta después del commit de respuesta y reabre el store: Resume termina sin
  otra petición al modelo ni herramientas.

Los fixtures de Resume usan HTTP local sin credenciales. No certifican la latencia
ni la disponibilidad de proveedores remotos. Tampoco certifican Markdown
incremental, intercalado visual de attachments, scroll automático ni ejecución
de herramientas a partir de fragmentos; el scroll de ejecución normal se verifica
por separado en las pruebas App de la primera sección.


## Controles humanos durante streaming

Runner y Resume pueden compartir `NativeManualCommandActor` con el host. El
Notify de una intención despierta al dueño del lease durante la solicitud de
modelo; ejecutar stdin/EOF no añade ToolStarted ni modifica el contenido del
modelo. Un error o cancelación del actor sigue pasando por el descarte del futuro
y el drenaje finito de deltas ya aceptados. El ledger conserva efectos pendientes
para inspección y puede bloquear un terminal; el host no inventa TurnCompleted.
La cerca de publicación reintenta sólo el mismo evento tras consumir/retirar una
intención. `native_manual_commands` verifica cuatro recorridos con HTTP SSE y
procesos reales: streaming normal, cancelación antes del efecto, reapertura y
Resume con stdin+EOF durante el stream. Un tool/hook/permiso en curso espera su
siguiente frontera segura; no hay ejecución concurrente a ese efecto.

## Regresión final del objetivo (2026-09-30)

`cargo test --workspace -- --test-threads=2` pasó con **1.714 pruebas aprobadas,
53 ignoradas, 23 suites y cero fallos**, en 73,05 s de ejecución de tests. Se
usaron dos threads para mantener acotados los recursos de fixtures de procesos,
HTTP y ventanas. La construcción de hosts de tests no consulta cuentas privadas
ni arranca discovery HTTP automático.

Las pruebas optativas de Codex se ejecutaron explícitamente por separado: Plan
pasó una vez y Build con lectura pasó dos veces, la última con el decoder SSE
nuevo. La exportación visual también se ejecutó por separado. Las otras pruebas
ignoradas conservan sus requisitos de plataforma, servicios o ejecución manual;
no se presentan como verificadas por esta corrida.

`cargo clippy -p openide-core -p openide-harness -p openide-ui --all-targets -- -D warnings`
pasó sin advertencias. `git diff --check` pasó. La evidencia remota y su alcance
están en [aceptación real](native-real-provider-acceptance.md#codex-aceptación-real-aprobada-del-streaming-2026-09-30);
las medidas compartidas están en [selectores y popovers](ui-selector-standard.md).

Clippy del desktop (`cargo clippy -p agents-window --all-targets`) terminó con
cero errores y 71 advertencias: 64 `collapsible_match`, dos `collapsible_if`,
dos diferencias de tamaño entre variantes y tres simplificaciones de expresiones.
El desktop todavía no cumple `-D warnings`. Esa limpieza general queda pendiente;
la aceptación del streaming no depende de silenciar esas advertencias.

El build final `cargo build -p agents-window` pasó sin errores ni advertencias.
Se inició `target/debug/agents-window` con las bibliotecas Wayland, xkbcommon y
Vulkan del entorno y se verificó que el proceso siguiera activo. La comprobación
de interacciones y pintura usa tests de `UiTree`; no se afirma aceptación manual
del sistema operativo o de todos los flujos OAuth desde la ventana.

## Regresión del 1 de octubre: MIME y permisos

La corrida final de `cargo test --workspace -- --test-threads=2` pasó con
**1.759 pruebas aprobadas, 53 ignoradas, 23 suites y cero fallos**, en 77,40 s.
Incluye la detección SSE con MIME ausente/incorrecto y BOM, los espacios entre
filas de permisos, la confirmación de acceso completo con acciones reales,
persistencia/migración/revocación y la ausencia del botón de consumo en el
composer. Las pruebas de elevación usan una frontera Polkit falsa y procesos
del usuario; no realizaron operaciones root.

La aceptación Codex Build pasó por separado en 20,49 s. La exportación de
`UiTree` pasó por separado y se inspeccionaron el menú compacto y el diálogo a
360×720 y 1400×900. Core, harness y UI pasaron Clippy estricto; el desktop pasó
Clippy normal y build. El supervisor iniciado sin privilegios salió con 125
antes de inicializar la ventana, como exige su contrato.

Clippy del desktop conserva exactamente **71 advertencias**: 64
`collapsible_match`, dos `collapsible_if`, dos `large_enum_variant`, una
`manual_clamp`, una `unnecessary_lazy_evaluations` y una
`unnecessary_unwrap`. Las ramas anidadas pertenecen principalmente a
`apply_action`; sus simplificaciones deben conservar el orden de guards y
efectos. Los enums son `BranchOperation` e `InspectionClient`; la advertencia
describe tamaño de almacenamiento y no demuestra una fuga de recursos. El
`unwrap` está protegido por un guard, y cambiar min/max por clamp requiere
considerar NaN. Esta revisión no hizo una refactorización general de esas
ramas ni atribuyó el fallo SSE a esas advertencias.

Se reabrió el binario final y se verificó una ventana OpenIDE mapeada, sin
ocultar, en Hyprland. La elevación real sigue pendiente del wrapper pkexec del
sistema descrito en [administración Linux](linux-administrator-commands.md).

## Coste del layout durante el streaming (4 de octubre)

La medición sintética `app::streaming_perf_tests::streaming_layout_benchmark`
usa el árbol completo de la ventana, las fuentes nativas de egui, un viewport
de 1280×800, 48 pares de mensajes históricos con Markdown y 24 actualizaciones
de una respuesta activa. No necesita proveedor, cuenta ni conversaciones
reales. Se ejecuta explícitamente con
`cargo test -p agents-window streaming_layout_benchmark -- --ignored --nocapture`.
Los tiempos corresponden al perfil de desarrollo sin optimización, en la misma
máquina; no representan latencia de red ni una garantía de FPS del compositor.

Antes del cambio, crear el árbol costaba 29,77 ms y su layout 2.094,41 ms para
los 24 frames: **88,51 ms por frame**. Conservar únicamente las mediciones de
ancho redujo las consultas al motor de texto de 27.420 a 593, pero no redujo
el tiempo total: la reconstrucción repetida de los párrafos formateados seguía
dominando. Conservar también sus líneas ya distribuidas redujo el layout a
696,07 ms y la construcción a 33,39 ms: **30,39 ms por frame**, una reducción
del 65,7% en esta prueba. Las consultas nativas bajaron un 97,8%.

`UiTree` conserva las líneas de los párrafos en una caché por superficie de
hasta 2.048 entradas y 4 MiB, con un máximo de 256 KiB por entrada. La búsqueda
comprueba el texto completo, estilos de cada segmento, ancho y altura; el hash
sólo sirve como índice. Un nuevo fragmento invalida su párrafo y las medidas
históricas siguen disponibles. Cambiar el motor de medida o las métricas
invalida ambas cachés. El host conserva el callback mientras el contexto y la
escala de pantalla sean iguales. La selección y los comandos de pintura se
reconstruyen con la fuente actual; no se conserva un frame interactivo viejo.

El avance visual utiliza un único instante por frame. Una primera pasada de
layout lenta ya no vuelve a avanzar el texto y a forzar otra pasada completa
en ese mismo frame. Los eventos siguen llegando en orden, el límite de drenaje
por frame no elimina la cola pendiente y los eventos terminales muestran el
texto completo inmediatamente. Las pruebas cubren identidad de contexto,
cambios de escala/fuente, texto exacto y Unicode, memoria acotada y finalización
sin repintados de presentación pendientes.

Las listas y citas de Markdown también miden su altura con el ancho asignado
por el contenedor flexible. Esto evita que una línea adicional al estrechar el
panel se dibuje sobre el siguiente bloque. La regresión redimensiona el mismo
árbol entre 140 y 480 píxeles y comprueba los límites de cada bloque.

La referencia local de Zed (`references/zed/crates/markdown/src/markdown.rs`,
`parse` y `reset`) conserva el resultado previo mientras agrupa las peticiones
de parseo pendientes. OpenIDE ya conservaba árboles Markdown; esta corrección
extiende la reutilización a las líneas medidas, que era la capa costosa
observada en la prueba.

### Segunda medición: medidas de contenedores y frame nativo completo

La siguiente corrección elimina la medición repetida del mismo nodo y las
mismas restricciones dentro de una pasada de layout. La caché temporal admite
hasta 16.384 medidas y se limpia al empezar la siguiente pasada. Los overlays
y las filas creadas por listas virtuales reciben ámbitos distintos: reutilizar
una dirección de memoria no permite reutilizar la medida de un árbol anterior.
Las dimensiones explícitas evitan recorrer descendientes cuando su tamaño
natural no puede cambiar el resultado. Una regresión compara el `Frame`
completo con las optimizaciones habilitadas y deshabilitadas, incluidos cambios
de ancho, filas virtuales, overlays y edición de un campo de texto.

Con el mismo perfil de desarrollo sin optimización, el benchmark de layout
pasó de 27,97 ms por frame en una nueva corrida previa a **12,54 ms** después
del cambio; el percentil 95 fue de 14,90 ms. Las diferencias pequeñas frente
a la primera corrida de 30,39 ms no se atribuyen al código: hay variación de
carga y frecuencia de CPU entre ejecuciones.

`native_frame_cpu_benchmark` amplía la prueba a 96 frames por escenario, después
de ocho frames de calentamiento. Incluye construcción/layout, el painter nativo
y la teselación de egui, con fuentes reales y el mismo historial/viewport. Los
tests de rendimiento se ejecutan secuencialmente para evitar interferencia:
`cargo test -p agents-window app::streaming_perf_tests -- --ignored --nocapture --test-threads=1`.

En desarrollo sin optimización, el streaming tuvo media 15,57 ms y p95
19,13 ms; el movimiento del puntero tuvo media 4,82 ms y p95 5,94 ms; abrir y
cerrar Environment tuvo media 7,73 ms y p95 14,87 ms. La pintura y teselación
individuales quedaron por debajo de 1 ms en p95. Estos resultados identifican
al layout como el coste restante y **no cumplen todavía el presupuesto de
8,33 ms de 120 Hz en todos los escenarios**. La prueba no incluye envío a GPU,
compositor del sistema, red ni latencia del proveedor. Un perfil optimizado
debe medirse separadamente con la misma fixture.

La fixture final mantiene visible el final del chat durante cada delta y
verifica ese seguimiento. Con ese cambio, la referencia dev es: streaming
19,57 ms de media / 23,18 ms p95; hover 4,74 / 5,71 ms; Entorno 7,53 / 14,27 ms.
La medición anterior de CPU pintaba el inicio del historial, por lo que no se
usa como comparación directa de este escenario. `OPENIDE_PERF_SCALE=2` permite
repetirlo con la escala de la pantalla de este equipo; la escala por defecto es 1.

La espera previa de un tooltip ahora solicita un repintado en su vencimiento,
sin producir frames intermedios vacíos. Las transiciones visibles conservan su
cadencia de animación y el contenido estático no inicia un bucle de repintado.

### Cadencia del host y ejecución optimizada

El host reutiliza la geometría del cuadro visible para dirigir eventos de
puntero. Los cambios del modelo, respuestas de workers, cambios de escala y
redimensionados la invalidan antes del siguiente hit test. Después de procesar
la entrada se recompone una vez el estado visual; mover el cursor ya no obliga
a construir el árbol de aplicación dos veces. Una actualización de 30 segundos
mantiene vigentes las etiquetas relativas al tiempo aunque no haya otra entrada.

Las animaciones visibles solicitan el siguiente frame mediante el scheduler de
egui, sin añadir una espera fija de 16 ms al tiempo que ya consumió dibujarlo.
La pantalla y el compositor determinan la cadencia real. La revelación del texto
usa pasos de 8,33 ms; reducir movimiento conserva el resultado inmediato. Los
workers sin novedades y la espera de tooltips mantienen repintados diferidos.

`scripts/build.py` usa por defecto el perfil `desktop`, que hereda las
comprobaciones del perfil de desarrollo y agrega `opt-level = 2`. El binario
está en `target/desktop/agents-window`; `--profile dev` sigue disponible para
depuración. Las mediciones dev y desktop se informan por separado para no
atribuir al caché de layout la mejora aportada por el compilador.

La validación del 4 de octubre con el perfil `desktop` y seguimiento del final
del chat dio, a escala 1, streaming **1,89 ms de media / 2,75 ms p95**, hover
**0,58 / 0,65 ms** y Entorno **1,11 / 1,81 ms**. Es la misma fixture que la
referencia dev anterior; esta diferencia mide el efecto del perfil optimizado.
El benchmark histórico de layout, sin pintura, dio 1,23 ms de media y 1,59 ms p95
con los callbacks estables.

A escala 2, como la pantalla 2880×1800 configurada a 120 Hz de este equipo,
el frame CPU completo dio streaming **2,93 ms de media / 4,95 ms p95**, hover
**0,69 / 0,92 ms** y Entorno **1,31 / 2,22 ms**. Los peores cuadros observados
en esos tres escenarios fueron 6,62, 1,02 y 4,40 ms respectivamente. Las 96
muestras por escenario quedaron bajo 8,33 ms después del calentamiento.
El viewport de la prueba sigue siendo 1280×800 puntos lógicos. Esto comprueba
el margen de CPU del caso medido, sin certificar GPU, compositor, proveedores
ni el coste de los polls del host con cualquier historial o subagente.

La regresión conjunta de `agents-window` y `openide-ui` pasó con **1.330 tests
aprobados y 38 ignorados**, tanto en dev como en desktop. Las pruebas de scroll
suave usan un reloj explícito a 120 Hz: la velocidad de la CPU no determina
cuánto tiempo ha avanzado la animación. Se ejecutaron por separado los seis
exports visuales del dock, incluidos Entorno, menú de sesión y confirmación
de eliminación, y las seis pruebas del script de compilación.

### Límites pendientes fuera del layout

La revisión del host encontró lecturas síncronas que la fixture anterior no
ejercita. En `app.rs`, `poll_subagent_events` llama a
`rebuild_live_subagent_views` por cada lote con eventos: esta reconstrucción
ejecuta `SessionStore::load` para cada hijo vivo y vuelve a leer su journal
completo, incluso cuando sólo llegó texto incremental. Si el hijo está
seleccionado, el mismo poll vuelve a cargarlo para reconstruir
`TranscriptProjection`. Además, `poll_subagent_sessions` llama a
`refresh_sessions` cada 500 ms mientras hay seguimiento de subagentes. Estos
caminos crecen con el historial y la cantidad de hijos, y siguen en el hilo de
UI. No se midió aquí su latencia ni se promete una cadencia global de 120 Hz.

Una mejora acotada pendiente es separar los eventos que alteran permisos,
títulos o estado de los que sólo agregan texto. Los primeros deben refrescar
las tarjetas de subagentes; los segundos pueden actualizar la proyección
seleccionada de manera incremental, conservando el snapshot de las tarjetas.
Antes de cambiarlo hay que verificar secuencias, huecos y desconexiones: la
lectura del journal sigue siendo necesaria para recuperar eventos no recibidos
y estados terminales. Esta propuesta no está implementada en esta revisión.

Los errores al inicializar el actor de comandos manuales sí respetan ahora el
intervalo existente de 500 ms. Antes, ese fallo dejaba vencido el timestamp y
repetía la lectura de metadata en cada frame. Los polls de catálogo, Git del
entorno, actor manual y diffs del chat también propagan sus cambios visibles
de carga o error para invalidar el árbol cacheado sin esperar otra interacción.

### Ritmo de lectura y actividad agrupada

La prosa del agente usa 14 px con una caja de línea mínima de 22 px. El código
inline conserva su altura natural y se centra dentro de esa caja, sin extender
su fondo por todo el interlineado. La selección sigue utilizando las cajas de
línea y conserva los rangos del texto original. La caché de párrafos incluye
la altura solicitada, además de fuentes, spans y ancho.

Los párrafos se separan 14 px; las listas, 4 px entre ítems; los encabezados
tienen 20 px antes y 8 px después. Las viñetas comparten la caja de la primera
línea del texto. El historial estrecho reserva 16 px de margen lateral, además
del espacio de la barra de desplazamiento. Estas medidas sólo afectan la
presentación de Markdown, sin modificar las métricas globales de los controles.

Se tomó como referencia el [Markdown de assistant-ui](https://github.com/assistant-ui/assistant-ui/blob/cf81a88583c952d1942df0644ac1782ed95090f3/packages/ui/src/components/react/assistant-ui/elements/markdown-text.tsx),
que distingue interlineado y márgenes por bloque. La implementación es nativa.
Los bloques completos conservan su árbol durante streaming; el renderer compara
también el espacio precedente antes de reutilizarlos.

Las lecturas y búsquedas consecutivas ahora tienen una cabecera compacta y
desplegable; el contrato de identidad, navegación y errores visibles está en
[Actividad de archivos](file-activity.md#integración-en-el-historial).

### Listados y archivos confirmados

Una exploración de `.` usa el nombre del proyecto en la cabecera. Las entradas
de carpeta ocupan una fila de 28 px con su ruta, sin repetir Archivo/Carpeta.
La vista abre seis resultados y permite mostrar los restantes; los nombres de
archivos navegan al editor y la cabecera conserva su acción de contraer.

Al terminar una escritura, el contenido registrado permanece visible aunque
falte la versión anterior. En ese caso se muestra como texto neutral y no se
inventan estadísticas de cambios. El inventario completo de una carpeta puede
probar que un archivo nuevo no existía aunque otra parte del proyecto haya
excedido los límites del snapshot inicial. Las notas y el código confirmado
comparten un margen interior de 8 px.

## Mensajes enviados durante un turno

Mientras el agente trabaja hay dos formas de enviar, y ninguna detiene el
turno:

| Tecla | Qué hace | Dónde espera |
| --- | --- | --- |
| Enter | **Poner en cola**: se envía como un turno propio cuando termine el actual | En la aplicación, por conversación (`turn_queue`); no sobrevive a un reinicio |
| Ctrl+Enter | **Redirigir**: el turno en curso lo lee en el próximo límite seguro, entre una respuesta y la herramienta que sigue | En la bandeja durable del turno (`enqueue_native_steer`) |

Los dos se ven sobre el compositor, en **Mensajes en cola**, cada uno con su
primera línea y la marca «Próximo turno» o «En este turno»
(`queued_messages` en `session_surface.rs`, `refresh_queued_messages` en
`app.rs`). Cada fila tiene:

- **Quitar**: lo saca de la cola. Uno redirigido sale de la bandeja con
  `SessionStore::withdraw_native_steer`, bajo el mismo bloqueo con que el turno
  lo toma: o lo tomó el turno o se retiró, nunca las dos cosas. El runner
  saltea uno retirado después de haberlo leído (`NativeSteerNotQueued`).
- **Editar**: lo trae de vuelta al compositor, si el compositor está vacío. Un
  mensaje redirigido con imágenes sólo se puede quitar.
- **Redirigir** (en uno del próximo turno): lo pasa al turno en curso.

Cuando el turno termina bien, el más antiguo de la cola se envía solo; lo que
el usuario esté escribiendo queda en el compositor. Si el turno se detuvo o
falló, la cola queda **en pausa** hasta que el usuario elija **Reanudar**, y lo
mismo si un mensaje de la cola no se pudo enviar. Un mensaje que el turno ya
tomó aparece en la conversación como mensaje del usuario y deja la cola.

Límites: hasta 16 mensajes en cola por conversación, sin adjuntos (los
adjuntos van con Ctrl+Enter); sólo en conversaciones nativas.

## Resumen de un turno

La fila de acciones de un turno terminado dice, después de cuándo terminó,
cuánto duró y con qué modelo, qué hizo el agente en una línea: «leyó 6
archivos, buscó 2 veces, ejecutó 3 comandos, editó 2 archivos»
(`activity_summary`, `message_actions.rs`). Se cuenta de las llamadas del
propio turno, que conservan su fila arriba: un archivo editado dos veces es un
archivo, seguir un comando o un subagente ya contado no suma, y un turno sin
herramientas no dice nada.
