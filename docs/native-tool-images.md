# Imágenes de resultados de herramientas en el motor propio

Cuando una herramienta MCP devuelve un bloque `image` (una captura de
`browser_take_screenshot`, una imagen de cualquier servidor MCP), el usuario la
ve en la tarjeta de la herramienta y **el modelo también la recibe**, si la
ruta y el modelo elegidos leen imágenes. Antes el modelo leía sólo una línea de
texto en su lugar.

Sólo se interpretan resultados de herramientas `mcp__…` con la forma de MCP
(`{"content":[…]}`); la salida de las herramientas locales nunca se analiza.

La otra fuente son las imágenes que el usuario adjunta a su respuesta a una
pregunta del agente (`ask_user`). No salen del texto del resultado: la
aplicación las registra con la respuesta en el journal de decisiones
(`answer_images`) y el motor las pasa al evento `ToolResultMedia` de esa
llamada, todas marcadas para el modelo. Desde ahí siguen el mismo camino, con
los mismos límites y la misma línea de texto donde el modelo no lee imágenes.
Una imagen que un modelo no recibiría (más de 3,75 MB u 8000 px por lado, o sin
medida legible) se rechaza al adjuntarla en vez de registrarse.

## Qué recibe cada proveedor

| Protocolo (rutas) | Dónde viaja la imagen | Qué documenta la API | Sin lectura de imágenes |
| --- | --- | --- | --- |
| Messages de Anthropic | Dentro del propio `tool_result`: `content` pasa a ser una lista con un bloque `text` y un bloque `image` (base64) por imagen. | `tool_result.content` admite bloques `text`, `image`, `document` y `search_result` ([Handle tool calls](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls)). | Línea de texto. |
| Responses (API de OpenAI y plan de ChatGPT/Codex) | Dentro del propio `function_call_output`: `output` pasa a ser una lista con `input_text` e `input_image` (data URL). | `output` es «string or array» de `input_text`, `input_image` e `input_file` ([referencia de Responses](https://developers.openai.com/api/reference/resources/responses/methods/create), [Function calling](https://developers.openai.com/api/docs/guides/function-calling)). | Línea de texto. |
| Chat Completions (OpenRouter, OpenCode, NVIDIA y demás compatibles) | El mensaje `tool` sigue siendo texto. Las imágenes van en **un mensaje `user` después del último resultado del lote**, como partes `image_url` (data URL) precedidas por `[Images returned by tool call <id>]`. | Un mensaje `tool` sólo admite texto; las imágenes sólo existen como partes `image_url` de un mensaje `user`. | Línea de texto. |
| Chat Completions de Mistral | No se envía. | Mistral rechaza un mensaje `user` inmediatamente después de uno `tool`. | Línea de texto, siempre. |

Las filas de Messages y Responses se leyeron en la documentación enlazada el
12 de octubre de 2026; la forma de Responses coincide además con la que envía
Codex (`FunctionCallOutputContentItem` en `codex-rs/protocol/src/models.rs`,
commit `33a0f766`). La fila de Chat Completions sigue el esquema publicado de
OpenAI, cuya página de referencia no pudo abrirse ese día: no hay una forma
documentada de devolver una imagen desde una herramienta en ese protocolo, y el
mensaje `user` posterior es la práctica habitual, no un contrato. El rechazo de
Mistral consta en un
[arreglo de Zed](https://git.secluded.site/zed/commit/b8c1b54f9eaf78d3bcf073f6495cfaeb8b3bf2b4),
no en la documentación de Mistral.

«Lee imágenes» es la misma capacidad `image_input` del modelo exacto que ya
habilita los adjuntos del usuario (`native_image_input_supported`). Un modelo
sin ella, una ruta sin herramientas y Mistral reciben, por cada imagen:

```text
[image 1 omitted (image/png): it is shown to the user, this model cannot receive images]
```

Con esto las rutas de Chat Completions con un modelo que declara `image_input`
también aceptan las imágenes que adjunta el usuario, que antes se rechazaban.

En el resultado, el bloque de la imagen se reemplaza por una línea que la
numera, y la imagen viaja a su lado en el mismo orden:

```text
[image 1: image/png, 1280x800 px]
```

## Límites

| Alcance | Límite | Motivo |
| --- | --- | --- |
| Por resultado | 4 imágenes y 8 MiB, PNG, JPEG, WebP o GIF de un cuadro | Los que ya tenía la vista (`MAX_TOOL_MEDIA_*`). |
| Por imagen, para el modelo | 3,75 MB (5 MB en base64) | El tope por imagen más estricto: modelos de Anthropic servidos por Bedrock o Google Cloud. La API de Claude admite 10 MB. |
| Por imagen, para el modelo | 8000 px por lado, leídos de la cabecera del archivo | Anthropic rechaza la solicitud por encima de eso. Una imagen cuya medida no se puede leer no se envía. |
| Por solicitud | 20 imágenes en total; las de herramientas ocupan lo que dejan las del usuario, hasta 16 | Con más de 20 imágenes Anthropic exige 2000 px por lado a todas y rechaza la solicitud si alguna lo supera. |
| Por solicitud | 15 MB de imágenes (20 MB en base64) entre las del usuario y las de herramientas | Deja lugar para el texto bajo los 32 MB por solicitud de Anthropic, que ahora también se comprueban antes de enviar. |

Los límites de Anthropic están en
[Vision](https://platform.claude.com/docs/en/build-with-claude/vision); los de
OpenAI (512 MB y 1500 imágenes por solicitud según
[Images and vision](https://developers.openai.com/api/docs/guides/images-vision),
leído el 12 de octubre de 2026) quedan muy por encima.

Nada se reduce ni se recorta: una imagen que no entra **se omite con una línea
que dice por qué**, igual que `browser_take_screenshot` falla en vez de
achicar. El usuario la sigue viendo.

```text
[image omitted (image/png, 4100000 bytes): larger than the 3.75 MB a model accepts for one image; it is shown to the user, the model does not receive it]
[image 1 omitted (image/png): an older tool image, no longer sent to the model; the user was shown it]
```

Siempre se envían las imágenes más recientes. Las antiguas salen de la
solicitud de a grupos de cuatro, para que el prefijo de la conversación que un
proveedor tiene en caché cambie una vez por grupo y no con cada captura. Por
eso una solicitud lleva entre 13 y 16 imágenes de herramientas cuando hay más
de 16.

Una captura de página completa muy alta entra por tamaño, pero el proveedor la
reduce a su resolución de trabajo (1568 px de lado mayor, o 2576 px en los
modelos de Anthropic desde la versión 4.7) y el texto deja de leerse. Para leer
una página sirve `browser_snapshot`; para verla, el viewport o un elemento.

## Historial, reanudación y compactación

- **Journal.** La imagen se guarda una sola vez, en el evento `ToolResultMedia`
  que ya alimentaba la vista. Ese evento ahora dice qué imágenes lleva el
  resultado para el modelo (`model_images`, sus posiciones). `ToolFinished`
  sigue guardando sólo el texto.
- **Replay y reanudación.** `replay_native` adjunta esas imágenes al mensaje
  del resultado, así que la solicitud siguiente y la que sigue a reabrir la
  conversación llevan el mismo mensaje que la original.
- **Journals anteriores.** Un `ToolResultMedia` sin `model_images` se carga
  igual y su resultado se reproduce como el texto que era: el modelo no pasa a
  ver imágenes que en su momento se le describieron como omitidas.
- **Cambio de modelo.** El historial conserva la imagen; cada solicitud decide
  según el modelo de destino. Volver a un modelo que lee imágenes las vuelve a
  enviar.
- **Presupuesto de contexto.** Cuenta como cualquier imagen: el conteo exacto
  del proveedor donde existe (Messages siempre; Responses con clave de API) y
  2000 tokens por imagen donde no (plan de ChatGPT y Chat Completions).
- **Recorte.** Cuando un resultado antiguo se reemplaza por
  `[Earlier tool result omitted from model context]`, su imagen se va con él.
- **Resumen semántico.** El pedido de resumen se arma con texto: cada imagen,
  del usuario o de una herramienta, es una línea
  `[image: image/png, N bytes, not included in this summary input]`. Antes el
  base64 de las imágenes del usuario entraba como texto en ese pedido. Un
  checkpoint que cubre el resultado lo resume y su imagen deja el historial del
  modelo; sigue en el journal para la vista.

## Lo que no cambia

La tarjeta de la herramienta muestra las mismas imágenes, texto y avisos. Sólo
cambia el texto del resultado en el inspector, que antes decía que el modelo no
recibía la imagen.

## Sin verificar con proveedores reales

Las pruebas arman la solicitud exacta de cada protocolo y la envían a un
servidor local que aplica sus reglas de estructura; ninguna llama a un
proveedor. Falta comprobar con cuentas reales:

- Que cada modelo describa una captura devuelta por
  `browser_take_screenshot`: un modelo de Anthropic por Messages, uno de OpenAI
  por Responses con clave de API, y el plan de ChatGPT (Codex).
- El mensaje `user` posterior a los resultados en Chat Completions, en
  OpenRouter y en NVIDIA con un modelo que lea imágenes.
- Los formatos WebP con pérdida y extendido, cuya cabecera se prueba sólo con
  bytes armados a mano (PNG, JPEG, GIF y WebP sin pérdida se comparan con el
  codificador real).

## Validación

```sh
nix-shell --run "cargo test -p openide-core -- tool_media image:: a_tool_result_replays"
nix-shell --run "cargo test -p openide-harness --lib -- tool_media history_adapter a_tool_result_carries a_function_call_output chat_sends_tool_images mcp_tool_media resumed_mcp_tool_media a_tool_image_is_budgeted the_summary_is_written"
nix-shell --run "cargo test -p agents-window -- the_size_a_model_limit a_chat_route_sends_images"
```
