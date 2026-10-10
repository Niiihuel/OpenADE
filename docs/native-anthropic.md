# Messages nativo

OpenIDE implementa `AnthropicMessagesBackend` en Rust, con su propio transporte,
journal y herramientas. No inicia Codex/OpenCode ni usa sus bibliotecas. El
protocolo `anthropic_messages` puede declararse con cualquier ID de proveedor en
el registro; el nombre `anthropic` es una definición incorporada, no una rama del
motor.

## Catálogo y selección

El adaptador consulta directamente `/v1/models`. La paginación usa las rutas y
parámetros declarados en `CatalogMapping`; la publicación requiere todas las
páginas dentro del plazo, bytes y cantidad agregados. Un cursor repetido,
duplicado de ID o página inválida conserva la última lista completa. No se usa
ETag para validar sólo la primera página de una lista paginada.

El listado declara límites de entrada/salida, esfuerzos y capacidades por ID.
Los valores nulos o cero permanecen desconocidos. Una respuesta actual con
campos mapeados ausentes, nulos o inválidos conserva esa observación: no hereda
capacidades, límites ni esfuerzos de un cache anterior. Las marcas de autoridad
también se guardan y reabren; las entradas antiguas sin esas marcas mantienen
su contrato previo. Esfuerzos se extraen del objeto
de niveles con `supported=true`, sin deducirlos del nombre. La API no anuncia
compatibilidad de herramientas cliente ni precios; `code_execution` no se
interpreta como soporte de las herramientas de OpenIDE. [Contrato de modelos](https://platform.claude.com/docs/en/api/http/models/list).

Autenticación: `x-api-key` se deriva únicamente de la variable declarada; headers
de versión/workspace son metadatos del registro. Catálogo e inferencia no siguen
redirecciones. El registro rechaza esquemas/headers que el transporte aún no
implementa. La versión de Messages aceptada es `2023-06-01`.

La selección usa IDs del listado. La respuesta debe declarar el mismo ID exacto;
un alias no resuelto explícitamente no se acepta como equivalente. No se asignan
capacidades de un modelo a otro. Automático omite `output_config.effort`; un nivel
explícito exige que ese ID lo anuncie. [Campo de esfuerzo](https://platform.claude.com/docs/en/build-with-claude/effort).

## Transporte e historial

System va separado. Herramientas propias se describen mediante `input_schema`;
sus resultados se agrupan en mensajes user con `tool_result`. Se preserva el
orden de text, tool_use, thinking firmado y redacted_thinking. El texto visible
de razonamiento no se inventa a partir de bloques opacos.

SSE valida índices, tipos, cierre de bloques, IDs y JSON de argumentos antes de
devolver herramientas. Exige `message_stop` y un motivo de cierre completo;
`max_tokens`, rechazo, `pause_turn`, error o EOF incompleto no autorizan efectos.
Uso de `message_delta` es acumulativo. Hay límites de 64 llamadas y 8 MiB de
respuesta retenida; no son límites de RSS. Retry sólo es posible antes de salida
observable; cancelación no completa la respuesta. [Secuencia SSE](https://platform.claude.com/docs/en/build-with-claude/streaming).

El envelope propio de `provider_output_items` conserva modelo, identidad de
endpoint y bloques íntegros. No reenvía elementos opacos de otro protocolo,
modelo o endpoint. Sigue pendiente una conversión entre protocolos demostrable;
esa ausencia produce un error explícito al intentar mezclar esos historiales.

## Presupuesto y UI

`max_tokens` es obligatorio. Sin tope de la ruta, OpenIDE solicita hasta 4096,
reducidos por el máximo de salida exacto declarado cuando existe. Ese valor es
una política de solicitud, no una capacidad anunciada del modelo. Settings
muestra el parámetro Messages correcto y conserva el tope por proveedor/ID.

`max_input_tokens` se conserva separado de una ventana total. Si sólo existe
máximo de entrada, el presupuesto aplica 90%/95% sobre entrada más framing; la
reserva de salida no ocupa ese máximo. Checkpoints guardan el límite independiente
y reabren sin inventar una ventana. El medidor compacto compara entrada con
entrada; la salida sigue visible en su fila, sin sumarse a ese porcentaje.

El motor consulta `/messages/count_tokens` con system, mensajes, herramientas,
imágenes inline y opciones originales antes de inferir cuando hay presupuesto.
También cuenta solicitudes sólo de texto y el pedido efectivo de resumen
semántico antes de compactar. Ese conteo y la inferencia del resumen comparten
el plazo/cancelación del servicio; superar el límite no publica un candidato.
Un fallo no se convierte en cero ni
autoriza saltarse el límite. [Conteo directo](https://platform.claude.com/docs/en/api/http/messages/count_tokens).

Selector, tooltip y presupuesto consumen la misma autoridad por ID exacto:
una omisión reciente no habilita niveles viejos ni un máximo retirado. El
conteo de uso guardado puede seguir visible como dato histórico, pero sin un
límite verificable no aparece un porcentaje o segmento de ocupación.

Imágenes exigen capacidad positiva del ID exacto. Los bytes/MIME/hashes del
journal se envían inline y sobreviven a reapertura. El protocolo soporta otros
tipos que OpenIDE aún no admite: PDF/documentos, audio/video, salida multimodal,
herramientas alojadas y gestión de contexto del proveedor permanecen pendientes.

ReadOnlyV1 congela auth/headers, opciones, capacidades, presupuesto y perfil de
petición. Reconstrucción y Resume no consultan Settings actuales ni serializan
la clave. Los headers opcionales y el límite de entrada se omiten en contratos
anteriores cuando están ausentes, conservando sus digests.

## Aceptación y límites

`native_anthropic_cycle` ejecuta Plan → Build → herramientas → Resume → resumen
semántico → checkpoint → reopen con HTTP/SSE local. Dos subprocesos salen sin
destructores después de `ModelResponseCommitted` y `ToolStarted`; recuperación
preserva el resultado y exige verificación manual de una lectura incierta. La
continuación conserva firmas, redacción opaca, IDs y resultados sin repetir la
lectura confirmada. Cuatro cambios de ruta/perfil se rechazan sin append ni
request. Resume sobre Idle después del checkpoint no agrega efectos.

Otro recorrido usa PNG inline, conteo real del fixture, reapertura y rechazo del
mismo input cuando supera el máximo independiente. Cuatro pruebas HTTP de compactación verifican rechazo sin inferencia/candidato,
conteo correcto de resumen en la región 90–95%, fallo/timeout/cancelación y
checkpoint reabierto. Las pruebas unitarias del
backend cubren HTTP/SSE, UTF-8 dividido, retry, cancelación, finales incompletos,
límites, opacos ajenos y redirecciones. Son pruebas del protocolo y recuperación
locales; no acreditan aceptación de una cuenta externa.

Tres pruebas del host crean el backend y reconstruyen ReadOnlyV1 con un ID de
proveedor arbitrario, verifican capacidades de imagen/thinking antes de HTTP y
presentan uso de una respuesta del runner en el componente compacto. La entrada
independiente de 100000 compara 10000 tokens de entrada como 10%; no agrega los
3000 de salida a ese porcentaje. Contexto, tooltip y el componente de tope se
ejercen con eventos del framework a 360/900 px; el guardado usa la acción real
del host. Esta prueba del contrato de componentes deriva la ruta del registro
abierto y no acredita navegación de una cuenta Anthropic en Settings: el
selector público expone sólo Codex, OpenRouter, OpenCode y NVIDIA, por decisión
del usuario. Cambiar el perfil invalida esa medición.
Las claves de estas pruebas son fixtures inyectadas, no credenciales de cuenta.

Las regresiones de catálogo usan GET HTTP real local, refresh completo y cache
reabierto. Comprueban niveles vacíos y familias de capacidades omitidas/nulas/
inválidas, límites retirados y rutas vecinas intactas. La proyección de metadatos
no conserva un rechazo viejo de herramientas si el nuevo listado ya no lo
declara. Un cache sin consulta nueva conserva su procedencia original.

En el entorno de esta implementación `ANTHROPIC_API_KEY` no está disponible.
No se enviaron inferencias externas a Anthropic. Aceptación con credenciales,
aliases resueltos, más tipos de bloque, UI de opciones de thinking y otras
plataformas siguen pendientes en el roadmap.
