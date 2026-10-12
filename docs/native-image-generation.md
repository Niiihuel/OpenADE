# Generación de imágenes en el motor propio

El agente nativo tiene una herramienta, `mcp__openide_image__generate_image`,
que genera **una** imagen a partir de un texto, la guarda como PNG dentro del
proyecto y la muestra en la tarjeta de la llamada. Gasta dinero del usuario y
escribe un archivo: **siempre pide aprobación**, también con «Permitir todo».

## Sin verificar con el proveedor real

Las pruebas hablan con un servidor local; ninguna llama a OpenAI. Antes de
confiar en la herramienta falta comprobar, con una cuenta real:

- **Que el plan de ChatGPT pueda generar imágenes por esta ruta.** Es la
  conexión principal de la app. El token del inicio de sesión se emite para
  `https://api.openai.com/v1` con el permiso `chatgpt.tokens.use.direct`, así
  que la llamada coherente es `POST https://api.openai.com/v1/images/generations`
  con ese token; nadie confirmó que el servicio la acepte. Si la rechaza, la
  llamada falla con «El plan de ChatGPT no permite generar imágenes con esta
  conexión (HTTP 403: …)» y no se guarda nada.
- **Qué modelos de la lista acepta cada cuenta**, en particular el plan de
  ChatGPT. La lista sale de los tipos del SDK de OpenAI
  (`types/image_generate_params.py`, leído el 12 de octubre de 2026), no de
  una consulta al proveedor.
- **El tamaño real de los PNG.** El modelo de la conversación recibe una
  imagen sólo hasta 3,75 MB y 8000 px por lado; no se midió si las imágenes de
  1536x1024 en calidad alta quedan por debajo. Por encima, el usuario la ve y
  el modelo lee una línea que dice por qué no la recibe.
- **El precio.** La app no lo conoce ni lo estima: la tarjeta sólo dice que la
  llamada puede tener costo.

## Cuándo se ofrece

En un turno Build del agente nativo, con «Modificar archivos» activo y una de
estas conexiones, en este orden:

1. Una clave de API de OpenAI (`OPENAI_API_KEY`).
2. El plan de ChatGPT (proveedor `codex`).

No se ofrece en Plan, en «Sólo lectura», a los subagentes, a los agentes
externos (Claude Code, Codex CLI) ni en las sesiones remotas, que arman sus
propios motores (`crates/openide-remote/src/config.rs`). Para decidir si se
ofrece sólo se mira que la conexión exista; la credencial se lee recién cuando
una llamada aprobada se ejecuta.

## Modelo y calidad

Los elige el usuario, no el modelo de la conversación, en Ajustes → Agente →
«Generación de imágenes». Valen para todas las conversaciones y se guardan en
`settings-preferences.json` (`image_model`, `image_quality`). Cambiarlos no
envía nada.

| Modelo | Calidades |
| --- | --- |
| `gpt-image-2` (por defecto) | baja, media, alta |
| `gpt-image-2.5-sunburst` | baja, media, alta, muy alta, máxima |
| `gpt-image-2.5-flare` | baja, media, alta, muy alta, máxima |
| `gpt-image-1.5` | baja, media, alta |
| `gpt-image-1-mini` | baja, media, alta |

La calidad por defecto es media. Si la guardada no existe en el modelo elegido
se usa la más alta que ese modelo admite; la guardada no se pierde y vuelve a
valer al elegir un modelo que la tenga.

## Aprobación

La tarjeta dice la ruta, el tamaño, la calidad, el proveedor y el modelo, que
puede tener costo, y el texto pedido (hasta 600 bytes):

```text
docs/assets/portada-faro.png · 1536x1024 · calidad media · OpenAI GPT Image 2 · puede tener costo — «Ilustración isométrica…»
```

El modelo y la calidad se leen de la preferencia al dibujar la tarjeta, y la
llamada vuelve a leerla al ejecutarse: lo que se aprueba es lo que corre. El
enlace «Cambiar modelo o calidad» abre Ajustes → Agente; al volver, la tarjeta
muestra la elección nueva. Una preferencia cambiada desde otra ventana de la
app no se refleja en una tarjeta de ésta hasta reiniciarla; la llamada usa la
guardada y el resultado dice cuál fue.

«Permitir en esta sesión» autoriza esa misma llamada, con los mismos
argumentos, durante la sesión. Repetida, falla antes de gastar porque el
archivo ya existe; si el archivo se borró, se genera otra vez sin preguntar.
La tarjeta no dice cuál de las dos conexiones paga: con una clave de API
presente, paga la clave.

## Argumentos

| Argumento | Regla |
| --- | --- |
| `prompt` | Obligatorio, de 1 a 4000 caracteres. |
| `path` | Obligatorio. Relativo al proyecto o absoluto dentro de él, terminado en `.png`. Las mismas reglas que `write_file`: sin `..`, sin enlaces, y la carpeta ya tiene que existir. |
| `size` | `auto` (por defecto), `1024x1024`, `1536x1024` o `1024x1536`. |

**Nunca reemplaza un archivo.** Si la ruta existe, la llamada falla antes de
enviar nada. Si el archivo aparece mientras la imagen se genera, la imagen ya
pagada se guarda al lado como `nombre-2.png`, `nombre-3.png`… y el resultado
lo dice.

## Resultado y límites

```text
Imagen guardada en docs/assets/portada-faro.png (1536x1024, 1.2 MB, GPT Image 2 · calidad media)
```

El texto deja escrito con qué modelo y calidad se hizo, que es lo que se
pagó. Lo acompaña la imagen, que sigue las reglas de
[las imágenes de herramientas](native-tool-images.md): se guarda una vez en el
journal y llega al modelo si su ruta lee imágenes.

| Límite | Valor |
| --- | --- |
| Imágenes por llamada | 1 |
| Espera | 30 s para conectar, 180 s en total |
| Respuesta del proveedor | 32 MiB; por encima no se guarda nada |
| Vista previa en la tarjeta | 8 MiB; por encima el archivo se guarda y el texto dice que no hay vista previa |

## Fallos

- Todo lo que puede rechazar la llamada (argumentos, ruta fuera del proyecto,
  archivo existente, carpeta inexistente) se comprueba antes de la solicitud:
  no se gasta nada.
- Un rechazo del proveedor devuelve el estado HTTP y su propio `error.message`,
  recortado a 300 caracteres. Nunca la credencial ni la respuesta completa.
- Una respuesta que no es base64 válido o no empieza como un PNG no se guarda.
- Si la imagen llegó y no se pudo escribir, el resultado es un error que igual
  trae la imagen, para no perder lo pagado.
- Cancelar el turno corta la espera. El proveedor puede terminar y cobrar
  igual una imagen que ya nadie recibe; lo mismo si la espera se agota.
- Una llamada interrumpida no se repite sola: queda como efecto sin confirmar
  (`crates/openide-core/src/replay.rs`) y el planificador la trata como una
  barrera, nunca en paralelo (`crates/openide-harness/src/tool_scheduler.rs`).

## Cambios de la sesión

Para la vista de cambios la llamada es un efecto opaco, como un comando: el
PNG nuevo no aparece en la revisión ni se puede revertir desde ahí, y la vista
agrega «Cobertura limitada: comandos, subagentes y otras herramientas no
tienen evidencia de cambios por archivo».

## No está hecho

- Otros proveedores (xAI, OpenRouter, Gemini) y la herramienta
  `image_generation` alojada de Responses.
- Editar una imagen, variaciones, fondo transparente, otros formatos o más de
  una imagen por llamada.
- Descubrir los modelos de imagen en el proveedor: la lista es fija.
- Elegir el modelo desde la propia tarjeta, sin pasar por Ajustes.
- Sesiones remotas, subagentes y agentes externos.

## Validación

```sh
nix-shell --run "cargo test -p agents-window -- image_ an_approved_call"
nix-shell --run "cargo test -p agents-window -- --ignored export_approval_visual_review export_image_generation_settings_visual_review"
```

La segunda escribe las capturas `image-approval-*` e `image-settings-*` en
`docs/ui-separation/shots/agent-tools/`.
