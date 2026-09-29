# Invalidación del catálogo MCP durante un turno

Cada turno Build descubre las herramientas MCP antes de llamar al modelo y
congela sus nombres y esquemas. Si un servidor emite
`notifications/tools/list_changed`, el catálogo anunciado al modelo deja de
ser seguro para llamadas posteriores. El turno actual conserva su digest,
rechaza nuevas llamadas a ese servidor y el siguiente turno abre una conexión
nueva para redescubrirlo.

El cliente stdio lee mensajes incluso cuando no hay una solicitud en curso.
El cliente HTTP reconoce la notificación tanto en respuestas SSE de solicitudes
como en el stream GET opcional de sesiones Streamable HTTP de 2025. Ambos
mantienen una señal de invalidación en el cliente antes de reenviar la
notificación a un canal acotado; un consumidor lento o ausente no puede dejar
habilitada una herramienta vieja. El adaptador `McpCompositeTools` consulta
esa señal antes de ejecutar cada llamada MCP. Una notificación durante
`tools/list` descarta el descubrimiento y no publica un catálogo mezclado.
Si una sesión HTTP de 2025 caduca después de anunciar herramientas, su
reemplazo también invalida el catálogo anterior aunque el servidor no envíe
un aviso.

Una llamada que ya se envió antes de recibir la notificación puede terminar;
el cliente no la repite. La señal sólo protege las llamadas siguientes. El
stream GET es opcional y debe iniciarlo el runtime; un servidor que responde
405 no ofrece avisos fuera de las respuestas a solicitudes. Los servidores
HTTP 2026 sin sesión no tienen stream GET independiente. Si se pierde el stream
legacy sin poder reanudarlo, el runtime invalida las herramientas del servidor
y comunica el error. La invalidación no modifica el catálogo ni su digest en
un turno activo.
