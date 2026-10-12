# Navegador del proyecto

La interfaz es nativa de OpenIDE. El contenido web lo procesa Chromium, en un
proceso que la aplicación inicia y maneja directamente por el protocolo
DevTools con su propio driver en Rust
([`crates/openide-browser`](browser-driver.md)). No intervienen Node, npm ni
Playwright: no hay otro proceso entre la ventana y el navegador. Compilada con
la opción `cef`, la aplicación usa en cambio un
[Chromium embebido](browser-cef.md) y deja este camino como reemplazo; lo que
sigue describe el camino por defecto. El contexto
es nuevo y aislado por conversación: no abre el perfil personal de Chrome. El
usuario y el agente comparten esa misma página, con una cola limitada y las
políticas de permisos del host. No se repiten automáticamente acciones
inciertas.

## Piezas

`apps/agents-window/src/browser_runtime.rs` es el actor de una conversación:

| Hilo | Qué hace |
| --- | --- |
| Trabajador | Es dueño del `Driver`, cuya API es bloqueante: atiende un comando por vez de una cola de 16, sigue la visibilidad de la vista y pide el cuadro sin pérdida. |
| Decodificador | Toma los cuadros del flujo del driver, descarta los que no son novedad y convierte los demás en píxeles. |
| Broker | Escucha en `127.0.0.1` con un token por contexto y atiende a los clientes MCP externos. |
| Lector del driver | Reparte respuestas y eventos del protocolo. Lo crea el propio driver. |

`browser_mcp.rs` es el proxy MCP: el mismo ejecutable con `--browser-mcp`.

## Arranque y temporales

Chrome hereda un entorno reducido (`PATH`, `HOME`, `USER`, `LOGNAME`, `LANG`,
`LC_ALL`, `XDG_RUNTIME_DIR`), sin claves de proveedores ni el token del
broker. Sus archivos van a un directorio privado `oid-XXXXXX` con modo 0700,
que el driver crea bajo `<datos>/browser-runtime` cuando esa ruta es corta y
bajo `/tmp` cuando no: Chromium crea sockets Unix por debajo de su raíz
temporal y una ruta válida para archivos puede superar el límite de
`sockaddr_un`. Nunca se usa el `TMPDIR` heredado, que puede pertenecer a un
`nix-shell` que ya terminó. Cerrar la pestaña mata el grupo de procesos de
Chrome y borra ese directorio antes de devolver el control; el cierre espera
hasta un segundo a que el trabajador termine. Si la aplicación muere sin
ejecutar destructores, Chrome ve cerrarse su tubería y termina solo, y el
directorio lo recoge el siguiente arranque. El detalle está en
[Driver del navegador](browser-driver.md#proceso-y-transporte).

### Qué navegador se usa

No se descarga ni se instala nada. Al iniciar cada contexto se busca, en este
orden:

1. El ejecutable de `OPENIDE_BROWSER_EXECUTABLE`, por ruta o por nombre en el
   `PATH`. Si la variable apunta a algo que no existe se informa eso; no se
   elige otro navegador en su lugar. Un navegador se inicia siempre por el
   nombre con el que se lo encontró y no por el destino de su enlace: el de un
   snap es un enlace a `snap`, que lee ese nombre.
2. Un Chrome, Chromium o Edge del sistema, en ese orden: por nombre en el
   `PATH` (`google-chrome-stable`, `google-chrome`, `chromium`,
   `chromium-browser`, `microsoft-edge-stable`, `microsoft-edge`; en Windows
   `chrome` y `msedge`) y después en los lugares habituales de cada sistema
   (`/opt/google/chrome`, `/opt/chromium`, `/opt/microsoft/msedge` y
   `/snap/bin`; `/Applications` y `~/Applications`; `Program Files`,
   `Program Files (x86)` y `LocalAppData`). De esos lugares sólo se probó el
   `PATH` de NixOS.
3. Salvo en NixOS, donde ese binario genérico no encuentra sus bibliotecas, el
   Chromium que una versión anterior de la aplicación haya descargado en
   `<datos>/browser-runtime/deps/node_modules/playwright-core/.local-browsers/`,
   la revisión más nueva.

Si no hay ninguno, el panel muestra «No se encontró un navegador», dice qué
instalar o qué variable definir y ofrece **Reintentar**, que abre un contexto
nuevo y vuelve a buscar. El sandbox de Chromium queda siempre activo.

El host pasa la escala de la ventana al iniciar el contexto: el
`pixels_per_point` de egui, acotado a 1–3 y redondeado a dos decimales. Chrome
se lanza con `--force-device-scale-factor`; con la emulación sola, los cuadros
del flujo saldrían a 1x. El viewport CSS sigue siendo el tamaño lógico de la
región visible, así que las coordenadas enviadas a la página no cambian. La
escala queda fija mientras viva el contexto: si la ventana pasa a un monitor
con otra, la página conserva la anterior y se muestra reescalada hasta
**Reiniciar navegador**.

El transporte del driver existe sólo para Unix. En Windows el panel dice «El
navegador todavía no funciona en este sistema» y no ofrece nada más que el
detalle. **Ese camino no se compiló ni se probó**, y el de macOS tampoco: el
entorno de desarrollo sólo tiene la biblioteca estándar de Linux, y las
separaciones por `cfg` se revisaron leyendo el código.

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
error de carga. Una página que no cargó reemplaza la vista de Chrome y elimina
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

Las teclas modificadoras junto con un clic (Shift + clic para extender una
selección, Ctrl + clic) no se transmiten, y la forma del cursor no cambia
sobre la página: ver *Lo que no se incluye*. El host envía ambos ejes del
trackpad y convierte Shift + rueda vertical en desplazamiento horizontal.
Pegar y confirmar texto de un método de entrada requieren que el viewport
visible tenga foco; durante una navegación no se envían al documento nuevo.
PageUp y PageDown llegan a la página.

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
hasta 64 KiB por conexión y responde hasta 4 MiB. La vista es un flujo de
cuadros de Chromium, descrito en **Vista en vivo**; no es un WebView: no
promete audio ni reproducción de video, formas del cursor ni la latencia de un
navegador de escritorio.

La consulta periódica de estado no borra el error de una navegación o de una
entrada que falló, ni el aviso de un flujo detenido: los conserva hasta la
siguiente acción explícita. La interfaz muestra una explicación breve con
detalle al pasar el puntero. La pérdida del proceso exige un contexto nuevo,
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

Mientras la superficie está visible, el trabajador mantiene el flujo de
cuadros del driver (`Page.startScreencast`, JPEG de calidad 80). Oculta, lo
detiene: Chrome deja de codificar y no llega ningún cuadro. Al volver a
mostrarse, la pestaña pinta su último cuadro, pero la página no recibe entrada
hasta el primer cuadro del flujo nuevo. Mostrarla siempre inicia un flujo
nuevo, también cuando el trabajador estaba ocupado con un comando y nunca
llegó a verla oculta. La URL, el título y el historial se
consultan cada 300 ms mientras está visible y sin comandos; es una sola
consulta al protocolo, sin imagen.

**Contrapresión.** Chrome espera el acuse de un cuadro antes de enviar otro, y
el driver lo acusa recién cuando el hilo decodificador lo toma. Si la vista se
atrasa, Chrome saltea cuadros: en todo el camino hay a lo sumo un cuadro
codificado esperando y uno decodificado que la ventana todavía no subió, y uno
más nuevo reemplaza al que nadie tomó. Las respuestas a comandos no comparten
canal con los cuadros. El hilo decodificador convierte JPEG o PNG en píxeles
(crate `image`) y el hilo de la interfaz sólo entrega el resultado a
`limn_egui::present_surface`, que escribe en la misma textura mientras el
tamaño no cambie.

**Nítido en reposo.** 200 ms después del último cuadro que mostró algo nuevo,
el trabajador pide una captura PNG a escala de dispositivo
(`Driver::capture_frame`) y la vista la muestra en lugar del JPEG. Una página
que sigue animando nunca llega a ese plazo; una que cambia menos de cinco
veces por segundo recibe una después de cada cambio. La captura se descarta si
la página cambió mientras se tomaba. Un cuadro idéntico al anterior no es
novedad: la propia captura hace que Chrome envíe uno, y sin esa regla una
página quieta se capturaría sin fin.

**Presupuesto.** Un cuadro tiene como máximo 3840 × 2160 píxeles (8 294 400).
El viewport más grande, 1600 × 1200, entra completo a escala 2. Por encima
(escala mayor que 2,08 con ese panel) el flujo se inicia con `maxWidth` y
`maxHeight`, Chrome reduce el cuadro y el host lo muestra en su tamaño lógico,
reescalado; en ese caso no se pide el cuadro sin pérdida, que tampoco
entraría, y lo que una captura del agente hizo saltear lo trae un flujo nuevo:
el trabajador lo reinicia cuando la página queda quieta. El límite de 4 MiB por línea del protocolo anterior ya no existe, y
con él la reducción de calidad del JPEG: el driver admite cuadros de 16 MiB.

**Superficie.** La página se pinta con `Element::surface` de limn: el cuadro se
ancla a la esquina superior izquierda de la región, alineado a la grilla de
píxeles, y cada píxel suyo ocupa un píxel físico. Un cuadro que difiere de la
región por un píxel de redondeo se recorta o deja un borde; no se estira. Un
punto es un píxel CSS, también mientras Chromium alcanza un cambio de tamaño.

**Capturas del agente.** `browser_take_screenshot` y el `browser_snapshot`
externo reciben un PNG a 1x. Con una escala distinta de 1 esa captura hace que
Chrome dibuje la página a otra escala por un instante, y el flujo lo ve:
medido con Chrome 150 a escala 2, llega un cuadro de la página reescalada (o
del tamaño del recorte, 448 × 1600 para una página completa) y después el
cuadro verdadero, idéntico al anterior. Una captura que va más allá del
viewport (página completa o un elemento) provoca lo mismo a cualquier escala:
a escala 1 llegó un cuadro de 121 × 480. La vista no muestra los cuadros de ese
momento: los saltea durante la captura y hasta 400 ms después, o hasta que
vuelve el cuadro que tenía. Lo que un cuadro salteado pudo ocultar lo trae el
cuadro sin pérdida que sigue.

**Cambio de tamaño.** Después de un resize el flujo se reinicia con los
límites del tamaño nuevo y la página recibe su cuadro sin pérdida a ese
tamaño. A escala 1 Chrome no envía por sí solo un cuadro tras el cambio.

**Pérdida del flujo.** Si el flujo no puede iniciarse, el trabajador lo dice
en el pie («La vista en vivo se detuvo…») y vuelve a pedirlo a los 2 s. Si lo
que falló es el navegador, se pierde el contexto. Una captura sin pérdida que
no se puede obtener (por ejemplo un PNG de más de 16 MiB) no detiene nada: el
JPEG queda y ese flujo no vuelve a pedirla; si Chrome todavía no tiene un
cuadro que dar, se pide de nuevo. **Actualizar vista** reinicia el flujo a
pedido.

**Mediciones.** Tomadas el 11 de octubre de 2026 a través del runtime real
(trabajador, driver, decodificador), sin la ventana, con la prueba
`real_chrome_measurements` compilada con el perfil `desktop`: Google Chrome
150.0.7871.46 a escala 2 y un viewport de 1280 × 800 (cuadros de 2560 × 1600),
carga de un minuto por debajo de 1 y ningún `rustc` en curso. Cada cifra es el
rango de cuatro corridas. Los tiempos hasta un cuadro son hasta el cuadro **ya
decodificado y listo para la ventana**, así que incluyen la decodificación;
las cifras anteriores del bridge se tomaban al llegar el cuadro codificado.

| Medida | Resultado | Cómo se tomó |
| --- | --- | --- |
| Lo que el navegador agrega al proceso de la aplicación, sin Chrome | 2,7 a 3,2 MB | PSS de `/proc/self/smaps_rollup` antes de iniciar el contexto y después de abrir una página y leer su árbol, con la vista oculta desde el arranque. Antes era un proceso Node de unos 107 MB. |
| Procesos de Chrome | 319 a 325 MB en 11 procesos | Suma del PSS del grupo de procesos de Chrome en ese mismo momento. |
| El proceso con la vista mostrando una página quieta, y animada | +61 MB y +65 MB | El mismo PSS con el flujo activo. Son los cuadros decodificados (16 MiB cada uno) y lo que el asignador retiene; esa parte de la vista ya existía con el bridge y no se midió entonces. |
| Arranque hasta tener el contexto listo | 350 a 371 ms; 3,0 a 3,6 s la primera vez | Desde `BrowserRuntime::start` hasta `started`. La primera vez después de que otro entorno usó Chrome tarda unos 3 s, como ya se había medido con el driver solo. |
| Arranque hasta el primer cuadro | 415 a 431 ms | Desde `start`, con la vista visible, hasta el primer cuadro decodificado. Con el contexto ya iniciado y oculto, mostrarlo da su primer cuadro a los 91–108 ms. |
| Cuadros por segundo, página animando | 33,7 a 36,0 | Cuadros decodificados contados durante 4 s en la página fixture con una animación CSS. |
| Cuadros por segundo, desplazamiento continuo | 31,2 a 32,1 | Un paso de rueda cada 16 ms como los envía la ventana, de a uno en vuelo, durante 4 s. El driver solo, con la misma página y el mismo desplazamiento y sin decodificar, da 32,7 a 33,2. |
| De la rueda al siguiente cuadro, desplazando | mediana 31 ms, p90 59–61 ms | Desde que se envía cada paso hasta el primer cuadro decodificado posterior. |
| Respuesta a la rueda de la ventana, desplazando | mediana 30–31 ms | Chrome contesta un paso de rueda cuando lo procesó, al ritmo de sus cuadros (29 ms con el driver solo); por eso van de a uno y los siguientes se suman en la cola. En reposo contesta en unos 2 ms. |
| De la rueda al primer cuadro, página quieta | mediana 63–64 ms, p90 64–65 ms | 20 pasos separados por 450 ms de quietud. |
| Decodificación, fuera del hilo de la interfaz | JPEG de 192 KiB: mediana 8,1 ms; PNG de 224–262 KiB: 4,4 ms | 40 repeticiones de `decode_frame` sobre un cuadro de esa misma página. |
| Entrega a la textura en el hilo de la interfaz | unos 3 µs por cuadro | `present_surface` sobre un contexto egui sin GPU. |
| Cierre | 12 a 17 ms, ningún proceso del grupo de Chrome | Desde soltar el runtime hasta que vuelve. |

Con la página quieta el límite es de Chrome: rasterizar y codificar 4
megapíxeles después de estar en reposo lleva unos 55 ms a esta escala, medido
también con el driver solo y con Playwright
([Driver del navegador](browser-driver.md#mediciones)). A escala 1 el mismo
viewport da 60 cuadros por segundo. La copia del cuadro a la GPU dentro del
renderizador (16 MiB por cuadro) no se midió, y **nadie verificó todavía
estas propiedades mirando la ventana real**.

## Motor: Chromium, Servo y lo que sigue

Se conserva Chromium. Lo que se veía borroso y lento no era el motor sino la
forma de mostrarlo: una captura PNG a escala 1 cada 300 ms sobre una pantalla
a escala 2. Medido el 11 de octubre de 2026 en una pantalla 2880 × 1800 a
120 Hz, con un viewport de 1280 × 800:

| Forma de mostrar la página | Cuadros por segundo | Nitidez |
| --- | --- | --- |
| Captura PNG periódica (anterior) | hasta ~3 | escala 1, estirada |
| Flujo en vivo a escala 1 | 60 | escala 1 |
| Flujo en vivo a escala 2 (actual) | 34–38 | nativa, 2560 × 1600 |

Servo se descarta por ahora. Está en crates.io desde abril de 2026 (0.1.0) con
versiones mensuales que cambian la API; en la 0.4.0 la mayoría de las páginas
de Gumroad no se dibujaban, y su árbol de accesibilidad está detrás de una
opción experimental. Playwright no lo maneja.
[Servo en crates.io](https://servo.org/blog/2026/04/13/servo-0.1.0-release/),
[julio en Servo](https://servo.org/blog/2026/08/31/july-in-servo/).

El flujo comprimido no llega a la frecuencia de la pantalla. El plan acordado:

1. **Hecho.** Vista en vivo a la escala de la ventana, en lugar de capturas
   periódicas.
2. **Hecho.** Un manejador propio en Rust sobre el protocolo de Chrome
   ([`crates/openide-browser`](browser-driver.md)). La referencia de diseño es
   [agent-browser](https://github.com/vercel-labs/agent-browser), que hizo esa
   misma migración. La conexión es por tubería privada, no por un puerto local.
3. **Hecho (11 de octubre de 2026).** La aplicación usa ese manejador detrás
   de los mismos comandos y ya no ejecuta Node, npm ni Playwright: desaparecen
   el proceso Node (unos 107 MB junto a los ~295 MB de Chromium, medidos aquí),
   el instalador y los dos scripts `.mjs`. El proxy MCP externo es el propio
   ejecutable.
4. **Hecha la primera mitad (11 de octubre de 2026), detrás de la opción de
   compilación `cef`.** Chromium embebido (CEF) con el mismo manejador, que
   habla con él por el mismo protocolo dentro del proceso. Compone por
   software y entrega cada cuadro sin comprimir: 60 cuadros por segundo a
   2560 × 1600 y 16 ms de la rueda al cuadro, sin tocar la GPU. Ver
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

Las pruebas rápidas forman parte de la suite central y no inician ningún
navegador:

```sh
nix-shell --run "cargo test -p agents-window -- browser_ approval_ui"
```

- `browser_runtime_tests.rs` maneja el runtime real (trabajador, driver,
  decodificador y broker) contra un extremo DevTools guionado en lugar de
  Chrome, por el punto de reemplazo del driver (`Driver::connect`). Cubre el
  arranque, la falta de navegador y su
  reintento, el orden de búsqueda del ejecutable, las referencias y cada
  clase de fallo, la entrada de la ventana con sus botones y clics, el flujo
  (cuadros sólo mientras es visible, acuse al tomarlos, cuadro repetido, cuadro
  sin pérdida al quedar quieta y sin repetirse, capturas del agente que no
  llegan a la vista, presupuesto de píxeles, reinicio tras un resize y flujo
  que no puede iniciarse), el catálogo, sus límites y el broker.
- `browser_mcp_tests.rs` ejercita el proxy sobre tuberías contra un broker
  local de prueba: transporte, solicitudes mal formadas, las cuatro en
  espera, cancelación, límites de entrada y de salida y cierre.
- `tests/browser_mcp.rs` ejecuta el binario construido con `--browser-mcp` y
  el entorno vacío: sirve MCP, sale con código 1 sin su configuración y no
  crea ningún archivo.
- `browser_host_tests.rs` verifica el foco, las entradas, la serie de clics,
  los botones derecho y del medio, el destino de reintento entre
  conversaciones, la conservación de ráfagas de entrada y el portapapeles con
  un actor de prueba sin navegador ni sockets. `browser_input_app.rs` contiene
  las regresiones de orden, agrupación y rechazo antes de modificar una cola
  llena. `browser_ui_tests.rs` cubre la barra a 240–900 px, los estados de
  error y la falta de navegador.

Las pruebas contra Chrome real están ignoradas por defecto y se activan con la
variable del ejecutable. Usan un perfil descartable bajo un directorio de
datos temporal y sólo visitan páginas que la propia prueba sirve en localhost:

```sh
OPENIDE_BROWSER_EXECUTABLE="$(command -v google-chrome-stable)" \
  nix-shell --run "cargo test -p agents-window -- --ignored real_chrome"
```

- Agente y cliente externo: arranque, navegación, captura con referencias,
  acción por referencia, capturas reducidas, fallos que son respuestas, página
  que no carga, y un cliente MCP que pasa por el binario real en modo proxy
  (lista de herramientas, captura con imagen, clic por referencia). Al cerrar
  no queda ningún proceso del grupo de Chrome ni su directorio.
- Vista en vivo a escala 2: cuadros al tamaño de dispositivo, ninguno oculta,
  página quieta que deja de enviar, capturas del agente que no llegan a la
  vista, cambio hecho por el agente que sí llega y cuadros al nuevo tamaño
  tras un resize.
- Entrada de la ventana: `:hover`, selección por arrastre, doble y triple
  clic, botones derecho y del medio, rueda, texto, teclas y portapapeles en
  dos pasos.

Los mismos escenarios corren sobre el Chromium embebido, compilando con la
opción `cef`: ver [Chromium embebido](browser-cef.md#validación).

Las mediciones de arriba se repiten con el perfil optimizado, sin otra
compilación en curso:

```sh
OPENIDE_BROWSER_EXECUTABLE="$(command -v google-chrome-stable)" nix-shell --run \
  "cargo test --profile desktop -p agents-window -- --ignored --nocapture real_chrome_measurements"
```

Validado el 11 de octubre de 2026 con Google Chrome 150.0.7871.46 empaquetado
por NixOS, con sandbox habilitado. Las pruebas del driver solo, también contra
Chrome real, están en [Driver del navegador](browser-driver.md#validación).

La revisión visual opt-in con fuentes nativas exporta vacío, carga, conexión
rechazada, página, menú, fallo de arranque y falta de navegador a 240, 320 y
640 px, sin lanzar procesos ni servidores:

```sh
nix-shell --run "cargo test -p agents-window browser_ui::visual_tests::export_browser_ui_visual_review -- --ignored"
```

Los PNG/SVG se escriben en `/tmp/openide-browser-ui-review`. El contenido de la
página es una fixture visual identificada.

`scripts/browser-desktop-e2e.py` y `scripts/dock-desktop-e2e.py` recorren la
ventana Wayland real (arranque con `TMPDIR` eliminado, navegación a un
servidor local, foco, texto Unicode, copia, corte y pegado). Su última
ejecución es anterior a la vista en vivo y a este cambio de driver: **no se
repitieron**. Ya no necesitan una instalación de Playwright; sí un build con
`--features editor-e2e`, Hyprland, `wtype`, `dotool`, `grim` y `wl-paste`.

```sh
nix-shell --run "cargo build --profile desktop -p agents-window --features editor-e2e"
python3 scripts/browser-desktop-e2e.py --binary target/desktop/agents-window \
  --output /tmp/openade-browser-native-e2e
```

## Lo que no se incluye

- **Modificadores con el clic.** Shift + clic y Ctrl + clic llegan a la página
  como un clic simple. El driver toma los modificadores de las teclas que tiene
  apretadas, y la ventana envía combinaciones completas (`Control+A`), no
  teclas sueltas: transmitirlos exige que el driver acepte modificadores por
  evento o que la ventana le envíe la tecla al bajar y al subir.
- **Forma del cursor.** El protocolo no avisa cuando la página cambia el
  cursor; habría que consultarlo en cada movimiento.
- **Descarga de un navegador.** Sin un Chrome, Chromium o Edge instalado el
  panel lo dice y no hace nada más. Con la opción `cef` el motor va embebido.
- **Windows.** Falta el transporte de tubería del driver; ver
  [Otros transportes](browser-driver.md#otros-transportes).
