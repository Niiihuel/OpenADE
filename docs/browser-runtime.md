# Navegador del proyecto

La interfaz es nativa de OpenIDE. El contenido web lo procesa un
[Chromium embebido](browser-cef.md) en la propia aplicación, que lo maneja
por el protocolo DevTools con su propio driver en Rust
([`crates/openide-browser`](browser-driver.md)). No intervienen Node, npm ni
Playwright, y no se busca ni se lanza ningún navegador del sistema. El contexto
es nuevo y aislado por conversación: no abre el perfil personal de ningún
navegador. El
usuario y el agente comparten esa misma página, con una cola limitada y las
políticas de permisos del host. Una CLI externa iniciada por OpenIDE tiene un
contexto aparte, uno por terminal, con las mismas piezas
([CLIs externas](browser-tools.md#clis-externas-iniciadas-por-openide)). No se repiten automáticamente acciones
inciertas.

## Piezas

`apps/agents-window/src/browser_runtime.rs` es el actor de una conversación:

| Hilo | Qué hace |
| --- | --- |
| Trabajador | Es dueño del `Driver`, cuya API es bloqueante: atiende un comando por vez de una cola de 16 y sigue la visibilidad de la vista. |
| Cuadros | Convierte lo que Chromium pintó desde su turno anterior en el cuadro, o la parte de cuadro, que toma la ventana. |
| Broker | Escucha en `127.0.0.1` con un token por contexto y atiende a los clientes MCP externos. |
| Lector del driver | Reparte respuestas y eventos del protocolo. Lo crea el propio driver. |

`browser_cef.rs` es la página embebida de un contexto: la abre, guarda su
último cuadro y recibe lo que Chromium pinta, en un hilo de CEF.
`browser_mcp.rs` es el proxy MCP: el mismo ejecutable con `--browser-mcp`.

## Arranque y temporales

CEF se inicia con el primer contexto de navegador del proceso, no con la
aplicación, y cada contexto abre en él una página propia, con sus cookies, su
caché y su almacenamiento en memoria. Cerrar la pestaña cierra esa página y
con ella todo lo suyo; el cierre espera hasta un segundo a que el trabajador
termine. Los archivos de CEF van a un directorio privado `oid-XXXXXX` con modo
0700 bajo `<datos>/browser-runtime` cuando esa ruta es corta y bajo `/tmp`
cuando no, nunca al `TMPDIR` heredado. Al salir la aplicación se cierra CEF y
se borra el directorio; si muere sin hacerlo, los procesos auxiliares terminan
solos y el directorio lo recoge el siguiente arranque. El detalle está en
[Chromium embebido](browser-cef.md#cómo-funciona).

No se descarga ni se instala nada al abrir el panel, y no hay ningún navegador
que elegir: el motor es parte de la aplicación. Si no puede iniciarse, la
pestaña lo dice con el motivo
([Si CEF no arranca](browser-cef.md#si-cef-no-arranca)). El sandbox de
Chromium queda siempre activo.

El host pasa la escala de la ventana al iniciar el contexto: el
`pixels_per_point` de egui, acotado a 1–3 y redondeado a dos decimales. La
página se crea a esa escala, hasta 2,07. El viewport CSS sigue siendo el
tamaño lógico de la región visible, así que las coordenadas enviadas a la
página no cambian. La escala queda fija mientras viva el contexto: si la
ventana pasa a tener otra, el contexto se reinicia en su dirección (ver
**Escala**, más abajo).

El motor sólo se compila para Linux. En cualquier otro sistema la pestaña dice
que el navegador embebido no pudo iniciarse y por qué; **ese camino no se
compiló ni se probó**
([Otros sistemas](browser-cef.md#compilar-y-abrir)).

## Interacción, errores y límites

La superficie usa una sola barra fija: atrás, adelante, recargar, dirección y
**Más opciones**. Enter abre la dirección; el menú reúne copiar dirección,
actualizar vista, copiar conexión MCP y reiniciar el contexto. Cada acción
secundaria explica su efecto en un tooltip. El título del sitio no se repite
debajo de la dirección. La página ocupa el área restante; no hay una segunda
barra de scroll nativa alrededor de Chromium. Un pie de 20 px conserva la
geometría de la página al mostrar progreso, cola o un rechazo de entrada.

La misma barra compacta se usa en IDE y Agentes: 32 px de alto, controles y
dirección de 24 px, iconos de 16 px y margen de 4 px. El viewport del runtime
descuenta estas medidas también antes de recibir el primer cuadro; mostrar
la terminal conserva la correspondencia entre el área renderizada y las
coordenadas enviadas al navegador.

El contenido tiene estados mutuamente excluyentes: vacío, cargando, página o
error de carga. Una página que no cargó reemplaza la vista de Chromium y elimina
su región interactiva, sea cual sea el texto del motivo. Para
`ERR_CONNECTION_REFUSED` en localhost se indica que hay que verificar el
servidor del proyecto y su puerto; la interfaz no inicia servidores.
**Reintentar** vuelve a navegar si el contexto sigue disponible y abre uno
nuevo sólo si no pudo iniciarse. Los errores de entrada, como una copia sin
selección, se muestran en el pie y conservan una página válida.

Las referencias de estructura, leídas sin copiar su implementación, son
`references/zcode/packages/ui/src/EmbeddedBrowserPaneParts.tsx` (barra y menú)
y `references/zcode/packages/ui/src/browser-use/BrowserViewportSurface.tsx`
(prioridad de errores y exclusión de la página fallida). OpenIDE conserva su
runtime propio y el contexto aislado por conversación.

El runtime acepta dirección HTTP/HTTPS sin credenciales incrustadas, historial,
recarga, clic, destinos por referencia, rol o selector, texto Unicode, teclas,
scroll y resize. Las URLs `file:`, `javascript:` y similares no se navegan.

La ventana envía además el puntero, en los mismos píxeles CSS:

- **Movimiento, botón presionado y botón soltado.** La página recibe estados
  `:hover` y arrastres (selección de texto, deslizadores); un arrastre que
  sale de la página sigue en su borde más cercano y la suelta se entrega
  siempre.
- **Doble y triple clic.** Una pulsación a menos de 500 ms y de 4 píxeles de
  la anterior es el siguiente clic de la misma serie: el segundo selecciona
  una palabra y es el `dblclick` de la página, el tercero selecciona el
  párrafo. La serie se cuenta al presionar, porque egui recién informa un
  doble clic al soltar.
- **Botón derecho y botón del medio.** Llegan como un clic completo en el
  lugar del puntero: la página recibe `contextmenu` o `auxclick`. No se abre
  un menú del sistema; una página con menú propio muestra el suyo.
- **Teclas modificadoras.** Shift, Control y Alt viajan con cada evento del
  puntero y de la rueda, como los tenía la ventana en ese cuadro. Shift + clic
  extiende la selección y los manejadores de la página ven `shiftKey`,
  `ctrlKey` y `altKey`. Sólo la ventana los envía: una herramienta del agente
  no puede nombrarlos.
- **Un enlace pedido en otra pestaña.** Un enlace con `target="_blank"`, un
  script que llama a `window.open` y Control + clic, el botón del medio o
  Shift + clic sobre un enlace piden otra pestaña u otra ventana. Cuando lo
  pidió un clic, los dos primeros abren una pestaña nueva del mismo navegador,
  que pasa a ser la que se ve; los clics con tecla o botón del medio abren en
  la misma página. Nunca se abre una ventana. El detalle está en **Enlaces que
  piden otra pestaña**, más abajo.

**Enlaces que piden otra pestaña.** La regla es la misma para el usuario y
para las herramientas del agente, cuyo clic es un clic:

| Pedido | Qué pasa |
| --- | --- |
| Clic en un enlace con `target="_blank"` o con el nombre de otra ventana | Se abre en una pestaña nueva, que pasa a ser la activa. La página que lo pidió queda en la suya. |
| Clic en algo cuyo script llama a `window.open(dirección)` | Lo mismo. El script recibe `null`: la pestaña nueva no tiene `opener`. |
| Control + clic, Shift + clic o botón del medio sobre un enlace | Chromium lo abre directamente en la misma página. |
| `window.open` o un clic hecho por script, sin un clic del usuario detrás | No abre nada: el script recibe `null`. |
| Un destino que no es `http` ni `https` | No abre nada. |
| Un formulario que envía a otra pestaña | Se abre su dirección por GET en la pestaña nueva, **sin los datos**, que no se envían. |
| Ya hay 8 pestañas | Se abre en la misma página. |

`browser_click` sobre uno de esos enlaces espera la página nueva y responde
con su dirección, su título y su árbol, como con un enlace común; la pestaña
que se abrió figura en `browser_tabs`. «Pedido por un clic» es
lo que Chromium entiende por eso: también lo que el script de la página hace
poco después de un clic (un `window.open` evaluado un segundo después de uno
se abrió).

La dirección se pide una sola vez: a Chromium se le pregunta antes de crear
la pestaña, y no la crea. Un enlace de un solo uso (un acceso por correo, una
confirmación) no queda gastado.

Lo que no llega: la página abierta no conoce a la que la abrió
(`window.opener`) y el driver no le pasa `Referer`; una ventana que un script
abre vacía para llenarla después (`window.open()` y más tarde
`ventana.location = …`) no abre nada.

**Control + rueda no hace nada.** En un navegador amplía la página. Acá no se
envía: ni desplaza ni amplía, y tampoco se mueve lo que haya detrás de la
página. Enviada por el protocolo desplazaba la página (medido), y un zoom
verdadero cambia cuántos píxeles CSS mide el viewport, que
es de lo que dependen las coordenadas del driver y la vista uno a uno. Control
apretado mientras un paso de rueda todavía se desliza no lo interrumpe. El
host envía ambos ejes del trackpad y convierte Shift + rueda vertical en
desplazamiento horizontal.

**Teclado.** Un carácter que escribe el teclado y que está en la distribución
estadounidense del driver (las letras sin acento, los dígitos, el espacio y la
puntuación ASCII) llega como una tecla que baja y sube, con su texto: la
página recibe `keydown` y `keyup`, que es lo que esperan sus propios atajos,
la búsqueda por letra de una lista y Espacio sobre un botón. Se comprobó que
la página oye esas teclas y que Espacio la baja; lo demás se sigue de eso.
Todo otro texto se inserta sin eventos de tecla (`Input.insertText`):
`ñ`, las vocales con acento que arma una tecla muerta, `ü`, `¿`, `¡`, `€`
(AltGr), un emoji, lo que confirma un método de entrada y lo que se pega.
Enter, Tab, Backspace, Delete, las flechas, Home, End, PageUp y PageDown van
como teclas, solas o con Shift y Control; un atajo nombra su letra como la
escribe el teclado (`Control+z`, y `Z` cuando también va Shift). Copiar y
cortar van por el portapapeles en dos pasos descrito más abajo, y pegar llega
como texto. Pegar y confirmar texto de un método de entrada requieren que el
viewport visible tenga foco; durante una navegación no se envían al documento
nuevo.

Lo que no llega: el texto todavía en composición no se muestra dentro de la
página, sólo lo confirmado; pegar no dispara el evento `paste` de la página; y
las teclas de función sin Control no se envían.

**Listas desplegables.** La lista de un `<select>` es un widget aparte de la
página, que Chromium pinta por separado: la vista la dibuja encima, se maneja
con el teclado (flechas, Enter, Escape), se elige con el puntero y se recorre
con la rueda. Ver [Chromium embebido](browser-cef.md#cómo-funciona).

**Cursor.** La ventana muestra el cursor que la página pide bajo el puntero:
mano sobre un enlace, barra de texto sobre un campo y los demás de CSS.

**Rueda y touchpad.** Los deltas propios de un touchpad llegan a la página tal
cual, pegados a los dedos. Un paso de rueda no salta: recorre unos 80 píxeles
(lo que un Chrome de escritorio bajo Wayland) deslizándose con la misma curva que las listas de
la aplicación (140 ms), enviado a la página cuadro por cuadro; con movimiento
reducido va de una vez. Medido en la ventana real a 120 Hz: doce pasos
seguidos mueven la página 960 píxeles sin un solo cuadro de más de 9 ms.

**Escala.** Una página conserva la escala con la que arrancó su contexto. Si
la ventana pasa a tener otra (se movió a una pantalla de otra densidad, o el
contexto arrancó en el primer cuadro de la ventana, que bajo Wayland todavía
dice 1), el contexto se reinicia en su dirección en lugar de quedar borroso.

**Qué gráfica dibuja la ventana.** La integrada, que es de la que cuelga la
pantalla en una portátil con dos (`WGPU_POWER_PREF=high` vuelve a la otra).
Medido en una portátil AMD + NVIDIA a 2880 × 1800 y 120 Hz con el navegador
animando: dibujando en la NVIDIA la ventana sostenía 120 cuadros por segundo
unos diez segundos y después 90–100, con 30–60 cuadros tardíos por segundo,
cuando esa placa bajaba su frecuencia; en la integrada sostuvo 120 durante
toda la medición, sin cuadros tardíos, y la NVIDIA pasó de unos 12 W a 3,5 W.

Cada respuesta a una herramienta contiene la URL, el título, el historial y,
cuando corresponde, el árbol accesible. La captura PNG del viewport ya no
acompaña a cada respuesta: se toma sólo para `browser_take_screenshot` y para
el `browser_snapshot` de un cliente MCP externo, que son quienes la leen. Las
respuestas a la entrada de la ventana no llevan imagen ni árbol: su resultado
llega por la vista en vivo. Se rechazan destinos ambiguos.

Para el agente, el árbol accesible lleva referencias de elemento (`[ref=e12]`)
y hay puntero, selección, casillas, arrastre, esperas, evaluación de
JavaScript en la página, consola, red, capturas y respuesta a diálogos. El
catálogo, el ciclo de trabajo, los límites de cada dato y lo que se dejó fuera
están en [Herramientas del navegador](browser-tools.md).

El driver dice qué significa cada fallo y el runtime lo traduce así:

| Clase del driver | Para el agente | Para la ventana |
| --- | --- | --- |
| `Tool`: referencia caducada, destino ambiguo, script que lanza, espera agotada | Resultado de error que el modelo lee. No es efecto incierto ni error de página. | No ocurre. |
| `Invalid`: la solicitud no es aceptable y nada llegó a la página (una tecla que no existe) | Igual que `Tool`. | Error en el pie. |
| `Failed`: llegó a la página y falló o agotó su plazo | Efecto incierto si la acción modifica. | Error en el pie. |
| `Navigation`: la página no cargó | Efecto incierto, como toda acción que modifica. | Estado de error de carga. |
| `Closed`: el navegador o su página ya no están | Se pierde el contexto. | **Reintentar** abre uno nuevo. |

La entrada de la ventana tiene una cola propia limitada a 128 comandos y
256 KiB, con hasta cuatro operaciones en vuelo. Agrupa texto Unicode y scroll
adyacentes sin cruzar clics, teclas o navegaciones. De los movimientos del
puntero en cola sólo sobrevive el más nuevo, con la misma regla; uno que no
entra en una cola llena se omite sin aviso. Movimiento y rueda van de a uno en
vuelo, responden sin estado de página y no cuentan como entradas pendientes ni
reconstruyen la ventana. La cantidad pendiente se muestra en la cabecera; si
se alcanza un límite se informa que la nueva entrada no se envió. Un rechazo
anterior al envío conserva las entradas aceptadas para despacharlas cuando
haya espacio.

Copiar devuelve la selección al portapapeles nativo sin leer el árbol ni
capturar nada. Cortar usa dos pasos: obtiene texto y un token, y sólo confirma
la eliminación después de entregar la copia al host y validar la conversación,
pestaña y foco. El driver vuelve a comprobar que el campo, valor y selección
no cambiaron. Admite corte en `input` y `textarea` editables, y copia de
selección normal del documento. Los campos de contraseña no permiten copiar ni
cortar; los de sólo lectura permiten copiar. El corte de `contenteditable`
todavía no está implementado. La selección transferida tiene un máximo de
16 KiB y no se retiene en el estado compartido con herramientas.

Las capturas tienen un máximo de 2 MiB y el árbol accesible, 64 KiB. Hay una
página por contexto, sin descargas ni ventanas emergentes. La cola Rust admite
16 comandos. El broker acepta cuatro conexiones a la vez, una solicitud de
hasta 64 KiB por conexión y responde hasta 4 MiB. La vista son los cuadros
que Chromium pinta por software, descritos en **Vista en vivo**; no es un
WebView: sin audio, sin WebGL y sin los códecs H.264, HEVC ni AAC
([límites](browser-cef.md#límites)).

La consulta periódica de estado no borra el error de una navegación o de una
entrada que falló: lo conserva hasta la siguiente acción explícita. La interfaz muestra una explicación breve con
detalle al pasar el puntero. La pérdida de la página exige un contexto nuevo,
para conservar la identidad de las herramientas pendientes. **Reintentar**
conserva el destino HTTP/HTTPS y lo abre una sola vez cuando el nuevo contexto
está listo, incluso si se cambió de pestaña mientras arrancaba. La barra
conserva esa dirección durante `about:blank` y ante una página de error
interna; sólo una navegación confirmada puede sustituirla. No se reproducen
clics, formularios ni escritura de la instancia anterior.

## Conexión MCP externa

**Copiar conexión MCP** copia la configuración de un servidor MCP por stdio:
el propio ejecutable de la aplicación con `--browser-mcp`, y en su entorno
`OPENIDE_BROWSER_BROKER` (`127.0.0.1:<puerto>`) y `OPENIDE_BROWSER_TOKEN` (64
dígitos hexadecimales, uno por contexto). Además viaja sólo lo que el sistema
necesita para volver a cargar ese ejecutable (`LD_LIBRARY_PATH` y sus
equivalentes): sin eso un binario compilado dentro de `nix-shell` no arranca
con el entorno vacío.

Ese modo se atiende antes que cualquier otra cosa en `main`: no abre una
ventana, no inicializa la GPU, no lee configuración y no crea archivos. Sólo
transporta solicitudes acotadas al broker de su contexto; el navegador sigue
siendo de la ventana.

- Una línea JSON-RPC de entrada admite 64 KiB; una más larga termina el
  proceso con código 1. Una respuesta admite 4 MiB.
- Hasta cuatro solicitudes esperan al broker a la vez; la quinta recibe
  «Browser request queue is full.». Una solicitud deja su lugar justo antes de
  que se escriba su respuesta, así que quien ya la leyó puede reutilizar el
  identificador y el lugar.
- Cada solicitud usa una conexión propia. `notifications/cancelled`, el cierre
  de la entrada estándar y el fin del proceso cierran esa conexión, que es la
  cancelación que el broker entiende: una acción en cola no llega a
  ejecutarse; una ya despachada pudo tener efecto. Una solicitud cancelada no
  recibe respuesta.
- El broker tiene 75 s para contestar.
- Las señales terminan el proceso sin manejador propio: el sistema cierra sus
  conexiones y el broker las ve como cancelaciones.

## Vista en vivo

Chromium compone la página por software, a la escala de la ventana, y entrega
cada pintura sin comprimir con el rectángulo que cambió. Un hilo de CEF copia
ese rectángulo al lienzo de la página; el hilo de cuadros del runtime
convierte lo pintado desde su turno anterior en una parte del cuadro que la
vista ya tiene, y el hilo de la interfaz sólo se la entrega a
`limn_egui::present_surface`, que escribe ese rectángulo en la textura. Un
cambio cuesta lo que ocupa y una página quieta no cuesta nada. Cómo viajan
los píxeles, cuándo va el cuadro entero y qué pasa con las listas
desplegables está en [Chromium embebido](browser-cef.md#cómo-funciona).

**Visible y oculta.** Mientras la superficie está oculta sus pinturas se
descartan, se suelta el lienzo y Chromium compone a 10 cuadros por segundo;
la página sigue corriendo para el agente. Al volver a mostrarse se le pide un
pintado completo: la pestaña conserva su último cuadro, pero la página no
recibe entrada hasta que llega ese cuadro entero. Se le pide también cuando el
trabajador estaba ocupado con un comando y nunca llegó a verla oculta. La
URL, el título y el historial se consultan cada 300 ms mientras está visible
y sin comandos; es una sola consulta al protocolo, sin imagen.

**Nada se encola.** Hay un solo lienzo y, esperando a la ventana, a lo sumo
un cuadro entero y la parte que cambió después; una parte que la ventana no
llegó a tomar se reemplaza por la siguiente, que abarca también su
rectángulo. Las respuestas a comandos no esperan detrás de los cuadros.

**Presupuesto.** Un cuadro tiene como máximo 3840 × 2160 píxeles (8 294 400).
El viewport más grande, 1600 × 1200, entra completo a escala 2. Una ventana
de más densidad crea sus páginas a escala 2,07, la mayor con la que ese
viewport entra, y la vista las muestra reescaladas.

**Superficie.** La página se pinta con `Element::surface` de limn: el cuadro se
ancla a la esquina superior izquierda de la región, alineado a la grilla de
píxeles, y cada píxel suyo ocupa un píxel físico. Un cuadro que difiere de la
región por un píxel de redondeo se recorta o deja un borde; no se estira. Un
punto es un píxel CSS, también mientras Chromium alcanza un cambio de tamaño.

**Capturas del agente.** `browser_take_screenshot` y el `browser_snapshot`
externo reciben un PNG a 1x. Chromium vuelve a pintar la página para cada
captura y, si va más allá del viewport (página completa o un elemento), la
dibuja distinta por un momento. La vista no recibe las pinturas de ese
momento ni de los 400 ms siguientes; lo que queda después sólo se muestra si
difiere de lo que ya tenía.

**Cambio de tamaño.** Primero cambia la vista y después la página, que pinta
entera a su tamaño nuevo; puede verse un cuadro intermedio.

**Actualizar vista** pide a pedido ese mismo pintado completo. La ventana lo
pide por su cuenta cuando su superficie ya no tiene el cuadro del que le
llega una parte.

**Mediciones.** Las de la vista, del arranque y de la memoria están en
[Chromium embebido](browser-cef.md#mediciones): 60 cuadros por segundo a
2560 × 1600 y 16–17 ms de la rueda al cuadro, por software. **Nadie verificó
todavía estas propiedades mirando la ventana real.**

## Motor: Chromium, Servo y lo que sigue

Se conserva Chromium. Lo que se veía borroso y lento no era el motor sino la
forma de mostrarlo: una captura PNG a escala 1 cada 300 ms sobre una pantalla
a escala 2. Medido el 11 de octubre de 2026 en una pantalla 2880 × 1800 a
120 Hz, con un viewport de 1280 × 800:

| Forma de mostrar la página | Cuadros por segundo | Nitidez |
| --- | --- | --- |
| Captura PNG periódica (anterior) | hasta ~3 | escala 1, estirada |
| Flujo de cuadros JPEG de un Chrome lanzado, a escala 1 | 60 | escala 1 |
| El mismo flujo a escala 2 | 34–38 | nativa, 2560 × 1600 |
| Chromium embebido, cuadros sin comprimir (actual) | 60 | nativa, 2560 × 1600 |

Servo se descarta por ahora. Está en crates.io desde abril de 2026 (0.1.0) con
versiones mensuales que cambian la API; en la 0.4.0 la mayoría de las páginas
de Gumroad no se dibujaban, y su árbol de accesibilidad está detrás de una
opción experimental. Playwright no lo maneja.
[Servo en crates.io](https://servo.org/blog/2026/04/13/servo-0.1.0-release/),
[julio en Servo](https://servo.org/blog/2026/08/31/july-in-servo/).

El flujo comprimido no llegaba a la frecuencia de la pantalla. El plan acordado:

1. **Hecho.** Vista en vivo a la escala de la ventana, en lugar de capturas
   periódicas.
2. **Hecho.** Un manejador propio en Rust sobre el protocolo DevTools
   ([`crates/openide-browser`](browser-driver.md)). La referencia de diseño es
   [agent-browser](https://github.com/vercel-labs/agent-browser), que hizo esa
   misma migración.
3. **Hecho (11 de octubre de 2026).** La aplicación usa ese manejador detrás
   de los mismos comandos y ya no ejecuta Node, npm ni Playwright: desaparecen
   el proceso Node (unos 107 MB junto a los ~295 MB de Chromium, medidos aquí),
   el instalador y los dos scripts `.mjs`. El proxy MCP externo es el propio
   ejecutable.
4. **Hecho (13 de octubre de 2026).** Chromium embebido (CEF) con el mismo
   manejador, que habla con él por el mismo protocolo dentro del proceso.
   Compone por software y entrega cada cuadro sin comprimir: 60 cuadros por
   segundo a 2560 × 1600 y 16 ms de la rueda al cuadro, sin tocar la GPU. Es
   el único motor: el Chrome headless que se lanzaba por una tubería privada,
   su búsqueda en el sistema y su flujo de cuadros JPEG se quitaron. Ver
   [Chromium embebido](browser-cef.md).
5. Los cuadros de CEF en una textura compartida de GPU, para llegar a la
   frecuencia de la pantalla sin copiar píxeles. Necesita la plataforma de la
   sesión en lugar de la headless y una superficie de limn que acepte una
   textura importada;
   [lo que queda](browser-cef.md#lo-que-queda-para-el-paso-de-gpu) lo detalla.

### Prueba de viabilidad de CEF (11 de octubre de 2026)

Prototipo sin ventana, CEF 154.0.34 con `cef-rs`, wgpu 30.0.1, GPU integrada AMD
(Mesa 26.1), sandbox activo, páginas `data:`. Resultados medidos:

| Pregunta | Resultado |
| --- | --- |
| Cuadros por segundo a 2560 × 1600 | 120,0 con `windowless_frame_rate = 120`; también 240 y más |
| Del evento de rueda al cuadro siguiente | mediana 5–7 ms |
| Importar el cuadro en wgpu sin copia | sí, con `texture_from_dmabuf_fd` de wgpu-hal; colores exactos |
| Costo por cuadro en el host | ~0,15 ms de CPU con la importación cacheada |
| Protocolo de Chrome dentro del proceso | sí, sin puerto: `send_dev_tools_message` y un observador |
| Memoria | ~430 MB en 9 procesos para una página simple |
| Disco a distribuir | ~310 MB (`libcef.so` sin símbolos pesa 272 MB) |
| Arranque | ~230 ms hasta el primer cuadro |

Lo que no salió como se suponía:

- El reloj interno alcanza: CEF 154 ya no limita `windowless_frame_rate` a 60.
  Marcar los cuadros desde el host agrega unos 20 ms de demora y pierde
  alguno; no conviene.
- Bajo Wayland CEF ignora la escala del dispositivo en una vista sin ventana.
  Se obtiene escala 2 con zoom de página al 200 %. La otra salida que se
  anotó acá, una vista en píxeles físicos más
  `Emulation.setDeviceMetricsOverride`, **no sirve**: al mirar el cuadro
  (11 de octubre de 2026, plataforma headless) la página queda a escala 1 en
  un cuarto del búfer, aunque diga `devicePixelRatio` 2. Aquella prueba había
  mirado una captura DevTools, no el cuadro.
- Con el proceso de GPU de Chromium en la NVIDIA no llegan cuadros acelerados.
  Chromium en la AMD con wgpu en la NVIDIA sí funciona, cacheando la importación.
- El CEF precompilado no reproduce H.264, HEVC ni AAC; sí MP3, VP8, VP9, AV1 y Opus.
- `libcef.so` se enlaza al iniciar el proceso (unos 53 MB aunque no se use).
  Cargarlo recién con la primera pestaña requiere una biblioteca aparte.

Sin verificar, porque exige una ventana visible: fluidez real en pantalla,
sincronía con el compositor y la integración con eframe (teclado, IME,
cursores, ventanas emergentes, cambio de tamaño). El prototipo y sus comandos
están fuera del repositorio, en `../openADE-spike-cef`. Lo que se construyó a
partir de él, y lo que resultó distinto, está en
[Chromium embebido](browser-cef.md).

## Validación reproducible

Las pruebas rápidas forman parte de la suite central y no inician Chromium:

```sh
nix-shell --run "cargo test -p agents-window -- browser_ approval_ui"
```

- `browser_runtime_tests.rs` maneja el runtime real (trabajador, driver y
  broker) contra un extremo DevTools guionado en lugar de una página, por el
  punto de reemplazo del driver (`Driver::connect_to_page`). Cubre el
  arranque, un contexto que no pudo iniciarse y su reintento, un motor que no
  puede iniciarse, las referencias y cada clase de fallo, la entrada de la
  ventana con sus botones y clics, el cuadro entero que se le debe a una vista
  que vuelve a mostrarse, que pide **Actualizar vista** o que cambió de
  tamaño, el catálogo, sus límites y el broker.
- `browser_cef.rs` prueba, sin iniciar CEF, cómo viaja lo pintado hasta la
  vista: ver [Chromium embebido](browser-cef.md#validación).
- `browser_mcp_tests.rs` ejercita el proxy sobre tuberías contra un broker
  local de prueba: transporte, solicitudes mal formadas, las cuatro en
  espera, cancelación, límites de entrada y de salida y cierre.
- `tests/browser_mcp.rs` ejecuta el binario construido con `--browser-mcp` y
  el entorno vacío: sirve MCP, sale con código 1 sin su configuración y no
  crea ningún archivo.
- `browser_host_tests.rs` verifica el foco, las entradas, la serie de clics,
  los botones derecho y del medio, las teclas modificadoras con el puntero y
  la rueda, Control + rueda, qué texto va como tecla y cuál se inserta, cómo
  distingue la pestaña un motor que no puede iniciarse de un contexto que no
  se inició, el destino de reintento entre conversaciones, la conservación de
  ráfagas de entrada y el portapapeles con un actor de prueba sin navegador
  ni sockets. `browser_input_app.rs` contiene las regresiones de orden,
  agrupación y rechazo antes de modificar una cola llena.
  `browser_ui_tests.rs` cubre la barra a 240–900 px, los estados de error y
  lo que dice la pestaña cuando el motor no puede iniciarse.

Las pruebas contra el Chromium embebido real están ignoradas por defecto. No
abren ventanas, no usan la GPU y sólo visitan páginas que la propia prueba
sirve en localhost. Qué cubren, cómo se corren y cuándo se validaron está en
[Chromium embebido](browser-cef.md#validación).

La revisión visual opt-in con fuentes nativas exporta vacío, carga, conexión
rechazada, página, menú, un contexto que no se inició y un motor que no
puede iniciarse a 240, 320 y 640 px, sin lanzar procesos ni servidores:

```sh
nix-shell --run "cargo test -p agents-window browser_ui::visual_tests::export_browser_ui_visual_review -- --ignored"
```

Los PNG/SVG se escriben en `/tmp/openide-browser-ui-review`. El contenido de la
página es una fixture visual identificada.

`scripts/browser-desktop-e2e.py` y `scripts/dock-desktop-e2e.py` recorren la
ventana Wayland real (arranque con `TMPDIR` eliminado, navegación a un
servidor local, foco, texto Unicode, copia, corte y pegado). Su última
ejecución es anterior a la vista en vivo, al driver y al Chromium embebido:
**no se repitieron**. Ya no necesitan una instalación de Playwright; sí un build con
`--features editor-e2e`, Hyprland, `wtype`, `dotool`, `grim` y `wl-paste`.

```sh
nix-shell --run "cargo build --profile desktop -p agents-window --features editor-e2e"
python3 scripts/browser-desktop-e2e.py --binary target/desktop/agents-window \
  --output /tmp/openade-browser-native-e2e
```

## Vista previa de un servidor del agente

Un comando que el agente inicia y que resulta ser un servidor abre su vista
previa en la pestaña Navegador de la conversación, según la preferencia
"Abrir la vista previa de los servidores que inicia el agente" de esta misma
sección de Ajustes. La detección de la dirección, la comprobación del puerto y
las tres opciones están en
[Comandos persistentes](persistent-commands.md#live-preview-of-a-started-server);
la herramienta `open_preview` del agente, en
[Herramientas del navegador](browser-tools.md#vista-previa-de-un-servidor-local).
La apertura reutiliza el mecanismo de los enlaces del chat: la dirección espera
en `browser_restart_urls` y se navega cuando el contexto está listo.

Una dirección de este equipo en la que nadie responde (un servidor que se
detuvo) se muestra como «No se pudo conectar con localhost:5173 — ¿sigue
corriendo el servidor?», con **Reintentar** y **Editar dirección**. «De este
equipo» es lo mismo que para los enlaces del chat (`chat_links::is_local`):
`localhost`, sus subdominios, el bloque `127.0.0.0/8`, `0.0.0.0` y `[::1]`.

## Perfil por proyecto: pendiente

Hoy cada conversación tiene un contexto propio en memoria y el pie de la
pestaña dice «Sesión aislada · Compartida con el agente». Que las cookies y el
`localStorage` sobrevivan entre conversaciones del mismo proyecto no se
implementó.

- **Cómo.** Dentro de un proceso: `root_cache_path` estable bajo el directorio
  de datos en lugar del directorio privado, y
  `RequestContextSettings.cache_path` por proyecto en vez del contexto sin
  ruta de hoy; dos navegadores con la misma ruta comparten el perfil sin
  conflicto. Entre procesos CEF toma su raíz en exclusiva: una segunda
  instancia de la aplicación tendría que usar un perfil efímero.
- **La clave.** SHA-256 de la ruta canónica del proyecto bajo
  `<datos>/browser-profiles/`. «Borrar datos de navegación de este proyecto»
  en el menú de la pestaña: con el perfil en uso, `Storage.clearDataForOrigin`
  y borrado de cookies por contexto; sin uso, borrar el directorio. El pie
  pasaría a «Perfil del proyecto · Compartido con el agente». Las pruebas de
  aislamiento (dos contextos no comparten nada, nada llega al disco) cambian
  de contrato.

Guardar y restaurar cookies por CDP al abrir y cerrar cada contexto se
descartó: la última conversación en cerrar pisaría la sesión más nueva de
otra, y `localStorage`, IndexedDB y los service workers no se restauran así.

## Lo que no se incluye

- **Zoom de página.** Control + rueda y el gesto de pellizco no amplían la
  página: ver *Control + rueda no hace nada*.
- **Ventanas con `opener`.** Las pestañas existen (tira bajo la barra de
  direcciones, herramientas `browser_tab*`), pero la que abre un enlace no
  conoce a la página que la pidió. Una ventana que un script abre vacía para
  llenarla después no abre nada, y un formulario que envía a otra pestaña
  pierde sus datos: ver **Enlaces que piden otra pestaña**. Las pestañas que
  abre el agente no se cierran solas al terminar su turno.
- **Teclas para todo el texto.** Lo que no está en la distribución
  estadounidense del driver (`ñ`, acentos) se inserta sin `keydown`, y una
  tecla lleva el código físico de esa distribución, no el del teclado real.
  Hacerlo exacto pide que la ventana entregue la tecla física de cada
  pulsación, que hoy limn no pasa.
- **Otros sistemas.** El motor embebido sólo se compila para Linux; en
  Windows y macOS la pestaña dice que no pudo iniciarse.
