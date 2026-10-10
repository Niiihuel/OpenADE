# Markers de actividad en conversaciones

El transcript usa el componente propio `openide_ui::Marker`. Los marcadores
presentan hechos del journal o una operación de UI realmente activa. El
spinner y el shimmer sólo se activan mientras esa operación está en curso;
la preferencia de movimiento reducido del framework mantiene el texto
completo y detiene la animación.

## Modelo y herramientas

`ModelRequestStarted` abre un marcador **Pensando** para la sesión, secuencia
y paso exactos. El primer `AssistantDelta` cambia el texto a **Generando
respuesta**; `ModelResponseCommitted` del mismo paso, el mensaje final o el
terminal del turno detienen la animación. Un error, una cancelación o una
reapertura sin resultado confirmado se muestran como tales. La salida de una
CLI por sí sola no inventa una solicitud nativa al modelo.

Cada `ToolStarted` tiene su marcador seleccionable para abrir los argumentos
y el resultado existentes. Las herramientas propias usan etiquetas de
lectura, búsqueda, escritura, comandos y subagentes; una herramienta MCP
desconocida conserva su nombre exacto. `PermissionRequested` muestra la
espera sin spinner de ejecución. `PermissionResolved` conserva la decisión
y `ToolFinished` confirma el éxito o el error. Un permiso pendiente reabierto
se distingue de una herramienta cuyo efecto es incierto. Un turno nuevo no
reactiva la animación de una llamada incierta de un turno anterior, y una
resolución manual sólo confirma el resultado de la llamada identificada.

## Presentación compacta del streaming

La conversación muestra un indicador de espera al final del contenido:
**Pensando** antes del primer texto visible, o **Preparando herramientas**
cuando recibe una entrada provisional que no tiene ya su propio indicador.
Cuando empieza a mostrarse la respuesta, se retira esa fila completa, incluido
su spinner: el texto no lleva un rótulo adicional «Respondiendo». Esto también
se mantiene al esperar la confirmación después del último fragmento; el turno
sigue activo y el compositor conserva Detener. Una nueva solicitud al modelo
puede volver a mostrar su espera aunque haya prosa de un paso anterior.
Pensando usa un cerebro estático de 16 px y un shimmer suave de texto en una
fila de 22 px. El brillo mezcla al 50% el color tenue y el principal; el icono
no gira. Con movimiento reducido, todo queda estático. Una ejecución real de
herramienta conserva su indicador propio después de confirmar la respuesta.

Herramientas, razonamiento, entradas provisionales y uso de tokens comienzan
cerrados, incluso si hay errores o una herramienta seleccionada. Sólo una
acción explícita cambia el disclosure; los eventos posteriores conservan esa
decisión hasta cambiar de conversación. La cabecera mide 32 px y trunca los
metadatos a su ancho. El razonamiento cerrado no añade una vista previa.
Las tarjetas de aprobación siguen mostrando sus acciones independientes.

El uso de tokens se presenta una vez al final de cada turno de usuario. Al
abrirlo se conserva cada respuesta con entrada, salida, caché, razonamiento y
estado registrados. La suma usa aritmética comprobada; un desbordamiento
muestra la cantidad de respuestas en lugar de inventar un total. Datos de una
respuesta no confirmada se marcan parciales. No se deducen costes ni tokens
faltantes. Un error durable de turno proporciona el motivo sin duplicar el
título rojo de actividad. El modelo y paso técnicos permanecen en el estado
interno, fuera del pie del compositor.

Los fallos de un turno aparecen una sola vez en su aviso de conversación.
Los códigos de cuota de ChatGPT se presentan en español al renderizar; el
error original permanece intacto en el journal y al reabrir la conversación.
El banner del composer se reserva para fallos locales previos a la admisión
del turno, como una carpeta inexistente o una configuración incompleta.

**Volver al final** comparte un botón circular blanco de 36 px, flecha oscura
centrada y tooltip, en chats nativos y CLI. Su acción conserva el seguimiento
del scroll; no envía mensajes ni abre paneles.

Referencia investigada: Zed `d284cbbc87b55d659303106ced627c8612369827`,
`crates/agent_ui/src/conversation_view/thread_view.rs` (indicador final,
disclosures, herramientas y contexto) y `entry_view_state.rs` (decisión manual
de apertura). Nuestra implementación usa openide-ui y conserva el journal.
La virtualización completa del transcript no forma parte de este cambio.

`conversation_stream_visual_tests.rs` cubre apertura/cierre por clic a 360 y
900 px, errores, razonamiento, estados terminales, movimiento reducido y
desglose por turno. `export_conversation_stream_visual_review` exporta fixtures
SVG/PNG de esos estados; los tests HTTP/SSE existentes comprueban el flujo
real del runner sin usar una cuenta externa.

## Compactación y rama de Git

La compactación manual muestra progreso únicamente cuando `ActiveCompact`
posee un worker para la sesión seleccionada y su proyecto canónico. Al recibir el resultado se vuelve
a leer el store; sólo un `CheckpointCommitted` con la secuencia recibida
permite presentar el checkpoint confirmado. El marcador histórico deriva
de ese evento durable y sobrevive a la reapertura. Un candidato escrito,
una respuesta del modelo que diga «compactado» o stdout no confirman un
checkpoint. La validación del payload, la compatibilidad y las decisiones
de Resume siguen siendo responsabilidad del replay nativo.

Durante ese trabajo se muestra «Compactando contexto…». El separador confirmado
dice «Contexto compactado» y reúne icono y texto entre reglas centradas; la
explicación del resumen y el número de evento quedan en el tooltip. La interfaz
no inventa porcentajes ni cantidades de tokens antes y después de compactar.

Un cambio de rama requiere dos revisiones de Git satisfactorias en el mismo
proyecto canónico y con la misma conversación seleccionada. El primer valor
es una línea de base. El marcador dice **Rama cambiada: A → B · observada en
Git** y no atribuye un checkout al agente. Se conserva sólo la observación
más reciente en RAM; cambiar el alcance la oculta y reabrir comienza con una
línea de base nueva. No se deduce ningún cambio de nombres de herramientas
ni de texto de stdout.

## Verificación y límites

`conversation_marker_tests.rs` verifica solicitudes y streaming reales del
protocolo de eventos, cortes en las fronteras del modelo, herramientas y
permisos con stores reabiertos, errores, incertidumbre y resolución manual.
También verifica candidato frente a checkpoint confirmado, IDs de acciones,
clics reales y texto completo a 360 y 900 px. El test de
`conversation_activity` usa un repositorio Git local real y un `git switch`
externo para comprobar la observación y la reapertura. El test existente
`settings_memory_compacts_real_http_turn_and_reopens_checkpoint_and_goal`
comprueba el progreso manual y el marcador confirmado después de compactar
contra el proveedor HTTP local y reabrir la aplicación.

El exportador opt-in `export_chat_marker_visual_review` produce SVG de
geometría a 420 y 900 px. No sustituye una aceptación de GUI en ejecución
ni una prueba con proveedores externos.

Pendiente: el runner automático no emite un evento de inicio de compactación;
su checkpoint confirmado sí aparece, pero la UI no adivina progreso previo.
Git no ofrece aquí un historial durable de checkout ni vigilancia continua;
se observa al actualizar la revisión de Git.

## Revisión de streaming y actividad · 7 de octubre de 2026

Al aparecer prosa visible se retira toda la fila «Respondiendo», incluido su
spinner. La espera anterior al primer texto, una nueva solicitud, los errores
y Detener conservan sus estados propios. Se verificó el flujo HTTP/SSE local
con deltas separados, cancelación y reapertura del historial; no se invocaron
proveedores externos.

La tanda focal aprobó 104 pruebas de aplicación (8 exportadores ignorados) y
29 de componentes compartidos. Incluye inspección por nombre, expansión por
el resto de la cabecera, identidad de conversación y fila, y paleta/Kbd a
anchos estrechos. Las suites de escritorio privado aprobaron además 9 grupos
de atajos y 17 de tooltips; sus informes están en
`/tmp/openide-kbd-keymap-native/report.json` y
`/tmp/openide-kbd-tooltip-native/report.json`. Estas cifras corresponden a la
selección de regresiones del cambio, no a toda la suite del workspace.
