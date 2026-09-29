# Proveedores, modelos y catálogo propio de OpenIDE

Decisión del 29 de septiembre de 2026: OpenIDE mantiene su propio catálogo de
modelos en Rust. El worker consulta las APIs oficiales de cada proveedor
admitido, valida las respuestas y conserva la última copia válida en una caché
local. La migración de la implementación y de la UI se documenta por separado
de la compatibilidad del harness nativo; una fila visible nunca autoriza por sí
sola una ejecución.

## Fuentes y alcance

| Proveedor | Fuente de descubrimiento | Límite de la información |
| --- | --- | --- |
| OpenAI | [`GET /v1/models`](https://developers.openai.com/api/reference/resources/models/methods/list) con la clave API del proyecto | Devuelve modelos accesibles para la credencial, pero no certifica herramientas, streaming, imágenes, esfuerzo ni presupuesto de contexto para este adaptador. |
| OpenRouter | [API de modelos](https://openrouter.ai/docs/api/api-reference/models/get-models) y, cuando corresponde, lista filtrada para la cuenta | Sus metadatos pertenecen a OpenRouter y pueden depender de rutas y preferencias de la cuenta; no heredan capacidades de otro proveedor por compartir nombre. |
| DeepSeek | [`GET /models`](https://api-docs.deepseek.com/api/list-models/) con la clave de DeepSeek | El proveedor declara IDs y ciertos metadatos; el harness valida por separado el protocolo y las opciones que realmente implementa. |
| Endpoint personalizado | ID y URL elegidos explícitamente por la persona usuaria | No hay descubrimiento ni límites confiables de forma predeterminada. |

El perfil externo de Codex CLI consulta los modelos que anuncia su propia
sesión. Sus credenciales y sus opciones no se copian al catálogo nativo. La
conexión nativa de OpenAI usa `OPENAI_API_KEY`; iniciar sesión en Codex CLI no
autentica las solicitudes Responses de OpenIDE.

## Contrato del catálogo

Cada entrada conserva el proveedor de origen, el ID exacto, el nombre y sólo
los campos que ese proveedor entregó. La caché guarda fuente, fecha de consulta
y última respuesta validada para mantener una lista útil sin conexión. Un
fallo de red o de parseo no sustituye una copia válida por datos incompletos.
Los datos almacenados son informativos y pueden quedar anticuados; la cuenta
y el endpoint verifican la disponibilidad efectiva al enviar la solicitud.

La selección nativa se persiste en `native-provider.json` sin credenciales.
Los adaptadores Rust de OpenAI, OpenRouter y DeepSeek, más el endpoint
personalizado, fijan la URL y el protocolo de ejecución. Una matriz local de
pares proveedor/ID revisados permite anunciar esfuerzos y límites conocidos
sin extrapolarlos por prefijo o por nombre. Para los pares sin evidencia, las
capacidades y los límites se muestran como desconocidos. La detección de
modelos nuevos no cambia automáticamente el conjunto ejecutable.

## Verificación pendiente

La API de listado y la matriz local no sustituyen una solicitud real de prueba
con la cuenta seleccionada. La aceptación de cada par requiere comprobar
autenticación, protocolo, streaming, herramientas, manejo de errores y, cuando
se ofrezcan, imágenes y esfuerzo. Los precios publicados por un proveedor son
referencia de catálogo; el gasto real requiere uso reportado y facturación de
la cuenta. No se infiere un precio ni una ventana de contexto cuando falta el
dato oficial.

La implementación del catálogo, su refresco y los estados visibles de Settings
se describen en [settings-capability-map.md](settings-capability-map.md); el
estado del harness está en [native-harness-roadmap.md](native-harness-roadmap.md).
