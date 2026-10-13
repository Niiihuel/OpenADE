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

## Inspección explícita en Settings

La página MCP ofrece **Inspeccionar herramientas** para cada servidor
habilitado. Sólo ese clic abre una conexión temporal, negocia el protocolo,
recorre todas las páginas de `tools/list` y cierra el cliente. No llama a
`tools/call`, no inicia un turno y no sustituye el catálogo congelado del
runner. Presenta la versión realmente negociada, la información del servidor,
los esquemas completos y sus campos adicionales en un panel desplazable.
Los errores de conexión, descubrimiento y cierre se muestran completos.

Cada servidor de la lista muestra cómo fue su última conexión: **Habilitado ·
sin probar**, **Conectado · N herramientas** o **No se pudo conectar** con la
primera línea del motivo. Tras una falla el botón pasa a llamarse
**Reconectar**: es la misma inspección. El estado no se guarda entre
ejecuciones y se olvida cuando la configuración de ese servidor cambia. Las
conexiones siguen siendo por turno: esto dice lo último que se vio, no una
conexión abierta.

La inspección carga la configuración guardada antes de iniciar y vuelve a
validarla al recibir el resultado. Un cambio del servidor, una configuración
ilegible o un cambio de proyecto descarta el informe; un resultado de una
consulta anterior tampoco puede reemplazar el estado de una consulta nueva.
Reabrir la ventana empieza sin informe y no reconecta automáticamente. La
operación corre fuera del hilo de UI, sigue actualizando su progreso sin
entrada del usuario y permite cancelar. Cada solicitud tiene un límite de
30 segundos, la conexión y el descubrimiento comparten 45 segundos y el cierre
dispone de hasta 35 segundos adicionales. Cancelar o cerrar la ventana libera
el proceso stdio o la sesión HTTP poseída sin bloquear la UI.
Si vence el plazo total durante un handshake legacy, primero se señala la
cancelación y se espera el cierre acotado del cliente; no se abandona de
inmediato el futuro que conoce la sesión parcial. Un cierre fallido o fuera
de plazo informa que no se pudo confirmar su liberación remota.

El hash SHA-256 del informe identifica exclusivamente los metadatos observados;
no es el digest durable del catálogo de un turno. La señal de cambio del
catálogo se conserva si llega durante esta conexión. Esta consulta temporal
no inicia el stream GET legacy para vigilar cambios posteriores y el informe
no certifica que el catálogo siga siendo el mismo al ejecutar un turno futuro.
Credenciales conocidas impiden publicar metadatos que las contengan; sus
valores también se eliminan de los diagnósticos antes de mostrarlos.

Las pruebas locales de `mcp_inspection` cubren HTTP 2026 y legacy 2025 con
paginación y cierre, stdio real con EOF, cancelación, timeout, stdout inválido,
destrucción del propietario, reapertura durante una solicitud pendiente,
configuración corrupta y respuestas obsoletas. Los tests de UI verifican el
clic real y la conservación de esquemas y errores completos en anchos de
360 a 900 px. No constituyen aceptación contra servidores externos ni una
prueba de OAuth en red o del vault de Windows.
