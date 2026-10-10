# Actividad de archivos

`openide_ui::FileActivity` presenta una operación sobre un archivo con una sola
identidad visual: nombre, directorio, estado observado, contadores opcionales y
contenido expandible. El componente no lee el disco ni calcula diferencias.
La aplicación suministra las instantáneas, las acciones y su estado real.

## Composición

`FileActivity::new(id, filename, body, palette)` acepta `.path(parent)`,
`.status(label)`, `.expanded(bool)`, `.running(bool)`, `.stats(additions,
deletions)`, `.actions(Vec<Element>)`, `.note(text)`, `.icon(Icon)` y
`.compact(bool)`. `.open_action(action_id, hint)` asigna al nombre una acción
de navegación independiente. El ID de expansión debe ser estable por llamada y archivo;
dos operaciones sobre la misma ruta no comparten su estado de expansión.

La presentación de edición utiliza un único marco de radio 8. La aplicación
configura `palette.surface` con `theme::CODE_SURFACE` y entrega el cuerpo sin
otra tarjeta ni un título de archivo repetido. El nombre dispone de más ancho
que el directorio; el tooltip muestra sólo la ruta completa y prefiere un lateral,
con ajuste automático si no hay espacio. No repite la acción ni el estado.
El estado y los contadores tienen una franja propia para conservar la identidad
del archivo en columnas estrechas. Los contadores grandes se abrevian y el
tooltip mantiene sus valores exactos.

La variante `.compact(true)` para lecturas cerradas usa una fila de 28 píxeles
sin marco ni fondo opaco: nombre, estado breve y apertura. Al expandirla, un único
marco suave une cabecera, código y control de contraer. El código conserva su
numeración registrada, un margen lateral de 8 px y desplazamiento independiente
con altura máxima de 180/240 px según el panel. «Leído» aparece sólo en la cabecera;
las lecturas vacías, pendientes o parciales conservan su indicación correspondiente.
El directorio completo permanece en el tooltip. Ambas variantes sólo incluyen
el cuerpo en el árbol cuando están abiertas. Las notas de error, permiso o
incertidumbre siguen visibles al cerrar el cuerpo.

El pie de contraer mide 28 px y centra un botón de 24 px con margen interior de
2 px; el marco añade otro píxel de separación. Sólo el icono recibe hover.
El cuerpo de lecturas, diffs y terminales expandidas deja 2 px debajo del contenido,
sin acumular otro espaciado inferior antes del pie.

El nombre responde a los eventos de botón normales de `UiTree`, incluidos Enter
y Espacio, y ejecuta la navegación independiente de `.open_action`. El enlace
se subraya al pasar el puntero o al recibir foco de teclado, sin fondo ni cápsula.
El resto de la cabecera —huecos, icono,
directorio, estado, contadores y flecha— expande o contrae el contenido como un
único control. Sólo el destino situado bajo el puntero recibe hover y activación;
abrir o inspeccionar no alterna también el desplegable. Sin acción de navegación,
el nombre también permite expandir los detalles. Las acciones compactas miden
24 píxeles y la cabecera de edición, 28 píxeles, con el estado en la misma fila.
Los títulos largos se acortan al ancho disponible; el directorio se desvanece.

Durante una operación se anima el texto de estado y el icono permanece estático.
Las ediciones abiertas muestran un `Skeleton` inicial de dos líneas mientras no
hay contenido visible. El primer contenido lo sustituye por completo, incluidos
los renglones vacíos, los cambios eliminados y las propuestas de eliminación.
El skeleton también desaparece al completar la entrada, esperar aprobación,
terminar, fallar o cancelar. `UiTree` conserva la fase por ID, detiene
la animación fuera de pantalla y respeta movimiento reducido. Los argumentos
parciales se etiquetan como vista previa, nunca como escritura realizada.

## Referencias observadas

La implementación es Rust original sobre `openide-ui`. Se estudiaron estas
fuentes locales para comprender la estructura y los costes de presentación:

- `references/zed/crates/agent_ui/src/conversation_view/thread_view.rs`:
  `render_tool_call` distingue edición y lectura; `render_tool_call_label`
  separa navegación al archivo y apertura; `render_diff_editor` reutiliza un
  editor de diferencias y muestra carga antes del primer rango disponible.
- `references/zed/crates/agent_ui/src/entry_view_state.rs`: expansión por
  identidad de llamada y editores retenidos por entidad.
- `references/zed/crates/acp_thread/src/diff.rs`: diferencias pendientes desde
  instantáneas, actualización asíncrona de rangos y finalización con contenido
  congelado.
- `references/zcode/packages/ui/src/ToolCallBlocks/renderers/edit.tsx` y
  `read.tsx`: tarjeta única para edición y fila ligera para lectura; la edición
  evita representar dos veces la misma vista previa.
- `references/zcode/packages/ui/src/ToolCallBlocks/renderers/EditInlineDiffContent.tsx`
  y `references/zcode/packages/ui/src/components/ui/highlighted-lightweight-diff-preview.tsx`:
  presentación ligera inicial, resaltado asíncrono y límites de contenido.

Estas referencias orientan la separación entre datos, representación y acciones.

También se revisaron las [capturas oficiales del panel de agentes de Zed](https://zed.dev/blog/parallel-agents).
Zed presenta las lecturas como filas compactas y conecta su salida mediante
indentación y un borde lateral en `render_markdown_output`. El marco completo
al expandir es una adaptación a las tarjetas propias de OpenIDE.

También se consultó el [changelog de Cursor 3.0](https://cursor.com/changelog/3-0),
que documenta la expansión y contracción de bloques durante el streaming. No
especifica las zonas de clic de los nombres de archivo: en OpenIDE el nombre
abre el archivo y el resto de la cabecera controla la expansión, según la
interacción solicitada para el producto.
No se incorporó código GPUI o React. El host debe calcular diferencias y preparar
contenido fuera de la construcción del árbol, conservar revisiones y acotar las
vistas previas largas; `FileActivity` no inicia ese trabajo al dibujar.

## Integración en el historial

Dos o más lecturas, listados o búsquedas consecutivas se presentan como
«Exploración · N», cerrado inicialmente. Su cabecera conserva el recuento y el
estado en curso. Al abrirlo aparecen las filas originales con sus enlaces y
desplegables independientes. Un detalle que el usuario ya abrió mantiene visible
su grupo. Los errores, permisos pendientes y resultados sin confirmar siguen
visibles aunque el grupo esté cerrado.

Al desplegar una actividad, un razonamiento o un grupo de exploración,
su pie muestra sólo un botón de flecha hacia arriba, con tooltip «Contraer».
El área de interacción y hover queda limitada al icono de 24 px, centrado en un
pie de 28 px con 2 px libres arriba y abajo; el resto es pasivo, sin divisor.
El cuerpo reserva sólo 2 px debajo del código para no duplicar esa separación.
Un degradado sutil se pinta detrás del control en
su espacio propio, sin cubrir la última línea del contenido. La terminal usa
el mismo pie cuando está expandida. Tiene identidad visual propia y ejecuta
la misma acción que la cabecera. Al cerrarlo desde el pie, el foco vuelve a la cabecera
y ésta se revela en el historial; no hace falta recorrer de nuevo todo el bloque.
El contenido cerrado deja de ocupar espacio, pero conserva las notas de error
y autorización. Se revisó este comportamiento en `openideFold.ts` del proyecto
anterior; la implementación nativa mantiene sus propios estados de actividad.

El borde inferior del historial usa un degradado de 32 px mientras quedan
mensajes debajo del viewport. Se pinta sobre el contenido y antes del composer,
sin cubrir su campo de escritura ni la barra de desplazamiento. Al llegar al
final desaparece para dejar íntegramente legible el último mensaje. Este efecto
no añade áreas de interacción ni cambia la altura del historial.

La identidad combina conversación y primera llamada. La clasificación de
operaciones no depende de si están preparando, ejecutando o terminadas: recibir
un resultado no cambia el grupo ni pierde su apertura. Un mensaje, turno,
razonamiento, edición u otra operación corta la secuencia. Sólo se omiten entre
filas los metadatos invisibles y la salida o medios de esa misma llamada.

Se consultaron los [grupos de assistant-ui](https://github.com/assistant-ui/assistant-ui/blob/cf81a88583c952d1942df0644ac1782ed95090f3/packages/ui/src/components/react/assistant-ui/elements/tool-group.aui.tsx)
y su [agrupación de partes consecutivas](https://github.com/assistant-ui/assistant-ui/blob/cf81a88583c952d1942df0644ac1782ed95090f3/packages/core/src/react/utils/groupParts.ts).
La presentación y resolución de acciones siguen siendo componentes Rust propios.

`file_activity_ui` conserva el desplegable de cada llamada durante la recepción
de argumentos, la ejecución y su resultado. Las lecturas son compactas y muestran
el contenido registrado con una sola columna de números de línea. Las ediciones
usan una única tarjeta: vista previa mientras llegan argumentos y diff confirmado
al completar la operación. No se añade otra fila «Cambios del archivo».

El encabezado de archivo mide 28 px y reúne nombre y cifras `+ / −` en una
sola fila. La ruta completa permanece en el tooltip del nombre; los estados de
éxito no añaden otra fila. Los errores y permisos pendientes permanecen visibles.
La numeración única no elimina los colores de las líneas añadidas o retiradas.
Un registro antiguo que sólo conserva el contenido escrito se representa como
contenido neutro; la limitación de comparación queda en el tooltip del encabezado.

Cada `write_file` y `apply_patch` nativo admitido captura las rutas afectadas
antes y después de la ejecución. Estos snapshots privados se vinculan a la
conversación, carpeta canónica, llamada exacta y evento `ToolStarted` persistido.
Permiten revisar esa edición aunque el inventario inicial fuese parcial o hubiese
un guardado manual anterior. Nunca se sustituyen con el contenido actual del disco.
El límite es de 16 rutas, 512 KiB por archivo y 2 MiB por lado; el registro global
comprueba una cuota de 256 MiB y 4.096 archivos por publicación. Las publicaciones
concurrentes de proyectos distintos pueden exceder marginalmente esa cuota. Un fallo de captura no cambia el resultado ni
repite la herramienta, y tampoco convierte contenido desconocido en un archivo
vacío. Al eliminar la conversación también se eliminan estas capturas.

Las tarjetas de terminal mantienen un encabezado de 28 px y una vista inicial
de seis líneas de salida. Expandir conserva una altura acotada y permite contraer
al pie. El comando completo y la carpeta quedan accesibles sin filas de metadatos
redundantes. La salida capturada y el resultado final se distinguen si difieren;
iniciar un proceso de fondo nunca se presenta como haberlo terminado.

Las dos ediciones más recientes del turno se abren inicialmente; cualquier
elección manual prevalece. Las vistas previas se limitan a 32 KiB, 160 líneas y
8 archivos, con caché de 32 entradas y 2 MiB. Los cuerpos cerrados no preparan
código. La proyección de cambios confirmados usa un worker y una caché de hasta
16 operaciones, separada por proyecto y conversación. Guardar manualmente un
archivo invalida la caché; los snapshots históricos verificados se pueden reconstruir. Nunca se usa el contenido actual del disco para
inventar qué escribió una operación anterior.

Las vistas de código en curso siguen las líneas nuevas si el lector permanece
al final. Desplazarse hacia arriba pausa ese seguimiento; volver al final lo
reanuda. Los diffs estáticos y las lecturas terminadas conservan su posición.

Las búsquedas locales y los listados de directorios conservan consulta, estado y
resultados navegables. El título de un listado abre esa carpeta en el explorador;
una página web abre su enlace. Las consultas conservan acceso al inspector y
nunca se interpretan como rutas. El resto de la fila despliega resultados. No
repiten un botón de código. La
búsqueda web alojada en Responses tiene eventos de presentación propios, sin
convertirlos en herramientas ejecutables. La fila mantiene su identidad durante
el streaming y se reconcilia con el resultado confirmado; también se reconstruye
desde el registro al reabrir la conversación. Los eventos de presentación se
limitan a 8 KiB y 8 fuentes. Desconexión, fallo o cancelación no se presentan como
éxito. Los enlaces se resuelven contra la fila vigente al pulsarlos y sólo abren
HTTP(S); los archivos deben pertenecer al proyecto actual.

La preparación de snippets no ejecuta herramientas. Un texto recibido a medias
se identifica como vista previa, y los errores de ejecución siguen visibles.
Las acciones de archivo se resuelven por conversación, identidad de fila e
índice al pulsarlas; las propuestas conservan el ID de su fila provisional.
Recibir una ruta permite ofrecer la apertura sin dar por realizada una escritura.
El nombre abre directamente el archivo en el editor o su captura provisional,
sin botones de apertura duplicados. La navegación valida conversación y fila
única al pulsar; las carpetas además deben existir y resolver dentro del proyecto.
Una propuesta provisional conserva su navegación a archivo o contenido
provisional, sin presentarla como una herramienta ya ejecutada.
Las animaciones sólo afectan al estado y al skeleton inicial de actividades en curso, respetan
movimiento reducido y se detienen fuera de pantalla.

## Validación

Revisión del 2026-10-08 (tarjetas de archivos y terminal): 493 pruebas de
`openide-ui` y 120 pruebas específicas del host aprobadas. Incluyen ejecución
local real con `AgentRunner`, journal y preparación del entorno, capturas por
operación, reapertura sin archivo actual, límites, interferencias externas,
colores del diff, navegación y foco/scroll durante streaming. Exports nativos
revisados en 240/360/900 px para archivo nuevo e histórico sin versión previa,
y en 360/900 px para terminal terminada, activa, expandida y con error.


Revisión del 2026-10-07 (enlaces, exploración y lectura): **88 pruebas específicas
de `agents-window` y las 487 pruebas de `openide-ui` aprobadas**. Incluyen
navegación de archivos/carpetas, rechazo de filas ajenas o ambiguas, foco y
expansión durante streaming, estados con error/permiso, interlineado, selección
y límites de caché. Se ejecutaron además los dos exports de cabeceras y
conversación; se revisaron el subrayado y los grupos cerrados/abiertos en anchos
estrechos y la prosa a 360/900 px con Ubuntu/Hack del host. Son fixtures locales,
sin solicitudes a proveedores reales. La compilación `desktop` terminó sin
errores; conserva advertencias de código sin uso fuera de este cambio.

Las pruebas del componente cubren anchuras de 240, 360 y 900 píxeles, icono
centrado, controles independientes por puntero y teclado, contenido desmontado
al cerrar, notas visibles, contadores grandes, lectura compacta y animación
estable o desactivada según el estado y movimiento reducido. La validación
integrada y visual del host se registra con el cambio de aplicación.

Validación local del 2026-10-04: 760 pruebas de `openide-core` y
`openide-harness`, más 1.388 de `agents-window` y `openide-ui` con el perfil
`desktop`. Las pruebas optativas de proveedores reales no se ejecutaron.
`export_file_activity_visual_review` exporta cinco escenarios en 360, 900 y
1.700 píxeles, con fuentes nativas y resaltado asentado, a
`/tmp/openade-file-activity-review`.
`export_file_activity_header_visual_review` añade una propuesta cancelada con
subrayado sobre nombre y hover de expansión en 240, 360 y 900 píxeles.

`file_activity_frame_benchmark` mide 48 bloques históricos, escala 2×,
8 cuadros de calentamiento y 96 muestras por escenario. En esta máquina, el p95
de construcción más layout fue 1,23 ms para streaming, 0,71 ms para hover y
0,45 ms para scroll. Son mediciones de CPU con fixtures: excluyen pintura, GPU,
compositor y latencia del proveedor, y no equivalen a garantizar 120 Hz de toda
la aplicación.
