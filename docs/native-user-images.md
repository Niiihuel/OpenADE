# Imágenes de usuario en el harness nativo

Una imagen de usuario se guarda como `UserImage`: MIME tipado, longitud, SHA-256 y bytes base64. Los eventos `TurnAdmittedWithImages` y `TurnSteeredWithImages` llevan la imagen completa en el journal. No guardan rutas locales ni dependen del archivo original al reabrir la sesión.

## Límites y validación

- MIME admitidos: PNG, JPEG, WEBP y GIF de un solo frame. Se comprueba la firma binaria; para GIF se recorren los bloques y se rechaza un segundo frame. Estos son los [formatos de entrada documentados para Responses](https://developers.openai.com/api/docs/guides/images-vision#image-input-requirements).
- Máximo 8 MiB por imagen, 8 imágenes y 24 MiB de bytes originales por mensaje. El journal conserva además su límite de 64 MiB por línea.
- `UserImage::from_bytes` calcula tamaño y digest. `verified_bytes` y `verified_data_url` verifican base64 canónico, tamaño, MIME y SHA-256 antes de usar la imagen. El append y la lectura del journal vuelven a verificar cada evento con imágenes; un registro alterado falla cerrado.
- La validación de firma y estructura GIF no equivale a decodificar por completo PNG, JPEG o WEBP. El proveedor puede rechazar un archivo que tenga firma correcta y contenido interno inválido.

## API de core

```rust
let image = UserImage::from_bytes(UserImageMime::Png, &bytes)?;
let event = sessions.admit_native_turn_with_images(
    session_id, turn_id, prompt, model, mode, &[image.clone()],
)?;
let queued = sessions.enqueue_native_steer_with_images(
    session_id, turn_id, input_id, text, &[image],
)?;
```

Los métodos antiguos de admisión y steering siguen creando los eventos de sólo texto. `AgentMessage.images` es opcional al deserializar, por lo que los journals previos conservan su lectura. El ack de steering sigue requiriendo el lease exclusivo y emite la variante con imágenes cuando la entrada las contiene.

Replay reconstruye las imágenes de cada mensaje de usuario desde su evento. Un checkpoint retiene el `event_sequence` del mensaje y, al reabrir, recupera las imágenes del evento original sin cambiar el formato ni el digest del checkpoint. El journal original debe permanecer disponible, como ya exige el replay de checkpoints.

El transporte de Responses está descrito en [responses-image-input.md](responses-image-input.md).

Cuando hay un límite de contexto verificado y al menos una imagen en la
solicitud, Runner y Resume consultan `POST /v1/responses/input_tokens` antes
de iniciar la respuesta. El conteo usa los mismos mensajes, imágenes en data
URL, elementos de replay, herramientas y opciones de razonamiento que la
solicitud de Responses. Se excluyen únicamente `stream`, `store` e `include`,
que controlan la respuesta y no forman parte de la entrada del modelo. El
presupuesto usa directamente el `input_tokens` devuelto; no convierte bytes
base64 ni presupone un costo fijo por imagen. Esto sigue la
[guía oficial de conteo de tokens](https://developers.openai.com/api/docs/guides/token-counting)
y el [contrato del endpoint](https://developers.openai.com/api/reference/resources/responses/subresources/input_tokens/methods/count).

Si el proveedor no entrega un conteo exacto o la consulta falla, la solicitud
con imágenes no pasa la puerta de un presupuesto verificado: se informa que el
costo visual es desconocido o se muestra el error de la consulta. La
compactación y la comprobación posterior vuelven a contar la solicitud
resultante. Los modelos sin ventana verificada conservan su comportamiento
anterior: pueden recibir imágenes, pero no hay garantía local de admisión por
tokens. Faltan pruebas con una cuenta OpenAI real y comprobar la disponibilidad
del endpoint en todos los modelos nativos anunciados. Las reglas locales de
[Images and vision](https://developers.openai.com/api/docs/guides/images-vision#calculating-costs)
no sustituyen esa comprobación porque dependen del modelo, tamaño y detalle.

## Vista de conversación

La proyección de la conversación conserva la fuente base64 de cada imagen y
la vincula al mensaje admitido o al steering confirmado. Al reabrir, la vista
se reconstruye desde esos eventos, incluso si el archivo original ya no está.
Sólo una imagen visible que falta en la caché se verifica mediante
`UserImage::verified_bytes()` y se decodifica. Un hash, MIME o contenido
inválido produce una vista previa no disponible; los bytes no verificados no
llegan al decodificador. Cada miniatura y su etiqueta accesible se dibujan con
el árbol propio de `openide-ui`; el backend registra la imagen y su texto
alternativo para accesibilidad.

La vista limita la fuente a 4096 píxeles por lado y 12 megapíxeles, comprueba
las dimensiones antes de decodificar y genera miniaturas de hasta 384 píxeles.
El decodificador recibe un presupuesto de asignación de 64 MiB (límite de
mejor esfuerzo de la biblioteca `image`). La caché retiene como máximo 16
miniaturas, unos 9 MiB de píxeles RGBA más memoria del GPU. La proyección no
guarda píxeles decodificados, pero conserva una copia base64 por imagen para
permitir acceso al desplazarse. El límite de 24 MiB de bytes originales por
mensaje sigue vigente; aún no hay un presupuesto acumulado para todas las
imágenes de una sesión extensa.
