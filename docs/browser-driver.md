# Driver del navegador en Rust

`crates/openide-browser` maneja la página de un Chromium embebido por su
protocolo DevTools (CDP), sin Node, sin Playwright y sin un crate CDP de
terceros. Reemplazó al proceso Node (`browser_bridge.mjs`) que usaba la
aplicación y ofrece las operaciones que anuncian las
[herramientas del agente](browser-tools.md), más la entrada cruda que
necesita una vista en vivo.

**Estado:** es lo que usa `agents-window` desde el 11 de octubre de 2026, y
desde el 13 sólo sobre el [Chromium embebido](browser-cef.md): el driver ya no
inicia ningún navegador. Lanzar un Chrome headless por una tubería privada, su
flujo de cuadros y la búsqueda de un navegador en el sistema se quitaron. El
actor que lo maneja, la vista y el proxy MCP están en
[Runtime del navegador](browser-runtime.md). En este documento, «el bridge»
es aquel proceso Node, que ya no existe: las comparaciones con él se conservan
como registro de lo medido.

## Piezas

| Módulo | Qué hace |
| --- | --- |
| `transport` | Mueve mensajes JSON completos. `MessageSink` y `MessageSource` son sus dos extremos, que implementa quien embebe Chromium. |
| `cdp` | Cliente fino: envía un método con parámetros a una sesión, espera su resultado y entrega los eventos. |
| `directory` | `PrivateDirectory`: un directorio privado y de ruta corta para los archivos del navegador, que usa el Chromium embebido. Sólo Unix. |
| `observer` | Corre en el hilo lector: consola, red, diálogos, navegaciones y marcos de otros procesos, en estado acotado. |
| `snapshot` | Convierte el árbol de accesibilidad de Chromium en el texto que lee el modelo y administra las referencias. Es puro. |
| `driver`, `actions` | La API pública: navegación, destinos, acciones con espera, evaluación, capturas, portapapeles y entrada cruda. |
| `scripts` | El único JavaScript: una función auxiliar que corre **dentro de la página**, en un mundo aislado. |

El llamador usa un hilo común y bloqueante, una solicitud por vez. Un hilo
lector reparte las respuestas y procesa los eventos donde llegan: nada se
encola, así que una página que escribe en la consola o pide recursos sin
pausa ocupa una cantidad fija de memoria aunque nadie esté consultando. Una
respuesta se interpreta una sola vez, en el hilo que la pidió y directamente
al tipo que necesita; una respuesta grande no demora los eventos que vienen
detrás.

## Página y transporte

El driver no inicia ningún navegador ni crea ninguna página. Quien embebe
Chromium crea la página y le entrega su extremo DevTools, que es un par de
objetos pequeños:

```rust
pub trait MessageSink: Send { fn send(&mut self, message: &[u8]) -> io::Result<()>; }
pub trait MessageSource: Send { fn receive(&mut self) -> io::Result<Option<Incoming>>; }
```

`Driver::connect_to_page(transport, ancho, alto, escala)` le pregunta a ese
extremo qué página es (`Target.getTargetInfo`) y se adjunta a ella con una
sesión propia. **No se abre ningún puerto TCP de depuración.**

- **Con CEF.** El `MessageSink` llama a `SendDevToolsMessage` y el
  `MessageSource` entrega lo que recibe el observador de mensajes DevTools a
  través de un canal acotado. Ese transporte está en `crates/openide-cef`
  ([Chromium embebido](browser-cef.md)), con los interruptores con los que
  corre Chromium.
- **Lo que queda a cargo de quien embebe.** La vida de la página, aislar su
  almacenamiento, rechazar las ventanas emergentes (el driver no pide
  descubrir otros destinos, que serían páginas ajenas) y rechazar las
  descargas. La escala también la fija quien embebe: el driver sólo la usa
  para devolver capturas a escala CSS.
- **Directorio privado.** `PrivateDirectory` es el que usa el Chromium
  embebido para sus archivos: `oid-XXXXXX`, modo 0700. La ruta completa no
  supera 48 bytes: Chromium crea sockets Unix debajo de su raíz temporal y
  `sockaddr_un` admite 108 bytes. Si la raíz pedida es profunda o ya no
  existe se usa `/tmp`; un directorio borrado no se recrea. Soltarlo lo
  borra. Si su dueño muere sin hacerlo, lo recoge el siguiente que se crea
  bajo la misma raíz: cada directorio lleva un archivo `driver.lock` con
  `flock`; uno con más de 60 s y sin candado pertenece a un proceso muerto y
  se elimina. Los directorios `oid-` que haya dejado el bridge de Node de una
  versión anterior, que no tienen ese archivo, no se tocan.
- **Una página.** Lo que un clic pidió ver en otra pestaña o en otra ventana
  se abre en la página:
  - La página anuncia que va a abrir una ventana (`Page.windowOpen`) antes de
    pedírsela al navegador, con la dirección y con si hubo un clic del
    usuario. Si lo hubo y la dirección es `http` o `https`, el driver navega
    la página ahí (`Page.navigate`). Es lo que cubre `target="_blank"`, una
    ventana con nombre y `window.open`, también desde un marco. Quien embebe
    rechaza la ventana, que no llega a crearse.
  - Lo que una página intenta abrir sin un clic detrás no abre nada.
  - La acción que provocó la navegación la espera como a la de un enlace
    común: `click` vuelve con el documento nuevo ya leído.
  - «Un clic del usuario» es lo que Chromium entiende por eso: lo que el
    script de la página hace mientras dura la activación que deja un clic
    cuenta como pedido por él. Visto en las pruebas: un `window.open`
    evaluado un segundo después de un clic en otra parte de la página se
    abrió. Cuánto dura esa activación no se midió.

## API

```rust
// `transport` es el extremo DevTools de la página que creó quien embebe.
let mut driver = Driver::connect_to_page(transport, 900, 700, 2.0)?;
driver.navigate("http://localhost:3000/")?;
let tree = driver.snapshot(&SnapshotOptions::default())?;   // texto con [ref=eN]
driver.click(&Target::Ref("e7"))?;
driver.fill(&Target::Role { role: "textbox", name: "Correo" }, "ada@example.test")?;
let png = driver.screenshot(Capture::Viewport)?;
```

| Grupo | Métodos |
| --- | --- |
| Ciclo de vida | `connect_to_page`, `resize`; soltar el driver lo desconecta de la página |
| Navegación | `navigate`, `back`, `forward`, `reload`, `info` (URL, título, historial, viewport) |
| Lectura | `snapshot`, `console_messages`, `network_requests`, `screenshot`, `evaluate` |
| Acciones | `click`, `click_at`, `fill`, `type_text`, `press`, `scroll`, `hover`, `select_options`, `set_checked`, `drag` |
| Esperas | `wait_for_text`, `wait_for_text_gone`, `wait_seconds` |
| Diálogos | `handle_dialog` |
| Portapapeles | `copy_selection`, `prepare_cut`, `confirm_cut` |
| Entrada cruda | `pointer_move`, `pointer_down`, `pointer_up`, `wheel`, `hold_modifiers`, `key_down`, `key_up`, `insert_text` |

Todo error es un `DriverError` con una clase, que dice más que el texto:

| `ErrorKind` | Significado |
| --- | --- |
| `Invalid` | La solicitud no es aceptable. Nada llegó a la página. |
| `Tool` | La respuesta de la propia herramienta: referencia caducada, destino ambiguo, script que lanza, espera agotada. Es el `output.error` del bridge; no hay efecto incierto. |
| `Failed` | La solicitud llegó a la página y falló o agotó su plazo. Si era una acción, su efecto es desconocido. |
| `Navigation` | La página no cargó (`net::ERR_… at <url>`). El driver sigue utilizable. |
| `Closed` | El navegador o su página ya no están. Hay que crear otro driver. |

Los textos que llegan al modelo o al usuario son los que daba el bridge, en
español: la integración no cambió lo que se lee.

`element_of(texto, referencia)` devuelve el rol y el nombre que una captura
imprimió para una referencia. Lo usa la tarjeta de aprobación de la
aplicación para decir qué es `e12`. Lee sólo la estructura de la línea: el
nombre se interpreta como el JSON que es y después sólo se recorren las marcas
entre corchetes que escribe el driver, de modo que un nombre o un texto de la
página que contenga `[ref=e12]` nunca coincide.

## Captura accesible y referencias

`snapshot` pide a Chrome su árbol de accesibilidad
(`Accessibility.getFullAXTree`) y una captura del DOM
(`DOMSnapshot.captureSnapshot`). Los roles, nombres y estados son los de
Chrome. El DOM aporta lo que la accesibilidad no trae: qué elementos tienen
caja y reciben eventos del puntero (sólo ésos llevan referencia), cuáles
muestran el cursor de mano (`[cursor=pointer]`), cuáles tienen un listener de
clic, el destino sin resolver de un enlace y el placeholder. No se marca ni se
modifica nada en la página. El texto mantiene la forma de
`ariaSnapshot({ mode: 'ai' })` de Playwright, que es la que describen las
herramientas.

- **Marcos.** Cada `<iframe>` visible incluye el árbol de su documento, sea
  del mismo proceso o de otro sitio (un marco de otro proceso tiene su propia
  sesión de protocolo). Hasta 32 documentos por captura.
- **Referencias.** `e12` en el primer documento; `f3e12` en cualquier otro,
  donde el prefijo numera documentos y nunca se reutiliza. Un elemento
  conserva su referencia mientras siga en su documento con el mismo rol y
  nombre. Una captura de la página completa reemplaza el conjunto de
  referencias vivas; una captura acotada sólo agrega. Antes de actuar se
  comprueba que el marco siga mostrando el documento para el que se emitió la
  referencia y que el nodo siga conectado. Si no, la respuesta es
  «La referencia e12 no corresponde a un elemento de la página actual. Tomá
  una captura nueva con browser_snapshot y usá sus referencias.» y la página
  no se toca.
- **Opciones para un presupuesto chico.** El motor propio entrega al modelo
  16.000 caracteres por resultado. `SnapshotOptions` permite achicar el árbol:
  `interactive_only` (sólo lo accionable y los contenedores con nombre que lo
  agrupan), `max_depth` y `selector` (el subárbol de un elemento). Los valores
  por defecto devuelven el árbol completo, acotado a 64 KiB y cortado en una
  línea entera; `truncated` avisa del corte.

### Diferencias con Playwright

Comparación hecha una vez contra `page.ariaSnapshot({ mode: 'ai' })` de
Playwright 1.63.0 con el mismo Chrome 150, sobre las seis páginas de
`crates/openide-browser/tests/fixtures`: formulario, listas anidadas, tabla,
marcos, controles propios (divs con clic, casillas con el input oculto) y la
página de herramientas. Sin contar la numeración de las referencias, 225 de
las 240 líneas de Playwright son idénticas, y 183 de sus 194 líneas con
referencia. Playwright da referencia a 194 elementos y el driver a 193: le
faltan dos (el contenedor `aria-hidden` y su botón) y agrega una (el `<span>`
con clic). Las diferencias, todas:

| Página | Playwright | Driver | ¿Importa? |
| --- | --- | --- | --- |
| formulario | `textbox "Clave": secreto` | `textbox "Clave": •••••••` | Sí, a favor: Chrome enmascara el valor de un campo de contraseña y el modelo ya no lo recibe. |
| controles | `generic [aria-hidden] [ref]` con su contenido | no aparece | Sí: lo que está a la vista pero marcado `aria-hidden` no figura en el árbol de Chrome. Un control dentro de un contenedor así sólo se alcanza por selector o por coordenadas. Es la única pérdida conocida. |
| controles | `text: Texto con clic sin cursor` | `generic [ref]: Texto con clic sin cursor` | A favor: un `<span>` con listener de clic conserva su referencia. Playwright lo reduce a texto. |
| controles | `slider "Brillo"` | `slider "Brillo": "30"` | No: se agrega el valor de un deslizador ARIA. |
| formulario | `option "Chile"` | `group "Otros":` con `option "Chile"` | No: se conserva el `<optgroup>`. |
| listas | `generic "Más detalles"` | `button "Más detalles"` y `[expanded]` | No: un `<summary>` se informa como botón con su estado. |
| listas | `blockquote: Una citaen dos líneas` | `…: Una cita en dos líneas` | No: un `<br>` separa palabras. |
| marcos, herramientas | `iframe [ref]` | `iframe "Marco" [ref]` | No: se conserva el título del marco. |

Diferencias que no dependen de la página:

- Playwright numera las referencias al recorrer el DOM y deja huecos; el
  driver las numera en el orden del texto. El prefijo de marco numera
  documentos en lugar de marcos.
- Los nombres accesibles los calcula Chrome y no la implementación propia de
  Playwright: pueden diferir en casos límite del algoritmo de nombres.
- `[level=N]` sólo se imprime en encabezados. Playwright también lo imprime en
  `treeitem`, `row` y `listitem` cuando llevan `aria-level` explícito.
- El marcador `[aria-hidden]` no existe en el driver.

## Destinos y acciones

Un destino es exactamente una de tres formas: `Target::Ref`, `Target::Role`
(rol y nombre exactos, tal como los imprime la captura, en el marco principal)
o `Target::Selector` (un selector CSS que coincida con un único elemento del
marco principal, raíces shadow abiertas incluidas). El destino se resuelve
antes de tocar nada: cero o varias coincidencias responden «El destino
coincide con N elementos…».

Antes de una acción sobre un elemento se espera, hasta 10 s y reintentando
con pausas de 0, 20, 100, 100 y 500 ms, a que el elemento esté conectado,
sea visible, no se mueva entre dos cuadros, esté habilitado y sea lo que un
clic en su centro realmente alcanza. Si el plazo se agota el error dice qué
faltaba, por ejemplo «Timeout 10000ms exceeded. Todavía `<div id="cover">`
recibe los eventos del puntero en su lugar.»

- `click`, `click_at` y `press` esperan a que se confirme la navegación que
  hayan iniciado y a que el documento nuevo termine de leerse
  (`DOMContentLoaded`), para que lo que se lea después no sea la página que se
  está abandonando ni un documento que todavía no tiene título ni contenido.
  Antes esperaban sólo la confirmación: en una de seis corridas de la prueba
  de enlaces, el clic respondió con la dirección nueva y el título vacío.
- `fill` selecciona el contenido y lo reemplaza con los mismos eventos de
  entrada que el teclado; los tipos `date`, `time`, `color`, `range` y
  similares reciben su valor directamente.
- `set_checked` hace clic donde lo haría el usuario: si el `<input>` real está
  oculto o tapado, en su `<label>` o en el contenedor visible más cercano. Es
  más de lo que hace Playwright, que en ese caso agota el plazo.
- `drag` mueve el puntero con el botón apretado y, si la página inicia un
  arrastre HTML, lo completa con los eventos de arrastre del protocolo.
- `evaluate` corre en el mundo de la página. Una expresión se evalúa; una
  función se invoca, con el elemento si se dio un destino, y se espera. El
  valor vuelve como texto JSON de hasta 16 KiB; lo que la página lanza vuelve
  como texto. Un script que espera una promesa que nunca se resuelve se
  abandona a los 10 s. Uno que nunca cede el hilo se **detiene** con
  `Runtime.terminateExecution`, y la página sigue funcionando; con el bridge
  ese caso perdía el contexto.

### Comportamientos de Playwright que no se replican

- Los motores de selectores propios de Playwright (`text=`, `role=`, `>>`,
  `:has-text()`): sólo CSS.
- `Target::Role` y `Target::Selector` buscan sólo en el marco principal, como
  `page.getByRole` y `page.locator`. Dentro de un marco se usa una referencia.
- La comprobación de qué recibe el clic se hace dentro del documento del
  elemento. No se verifica que el `<iframe>` que lo contiene esté tapado por
  algo del documento que lo rodea.
- `wait_for_text` no busca dentro de raíces shadow ni de marcos.
- `select_options` no espera a que una opción deshabilitada se habilite: la
  trata como ausente hasta agotar el plazo.
- Desmarcar un botón de radio no da el error específico de Playwright; falla
  porque el clic no cambia su estado.
- El clic del agente acepta botón, doble clic y modificadores
  (`click_with`, `hold_modifiers`); no hay `force`, `trial` ni `position`.
- El teclado es la distribución estadounidense. Un carácter fuera de ella
  (`ñ`, `é`) se escribe a sí mismo sin código de tecla; no hay teclado
  numérico.
- Un valor evaluado se serializa con `JSON.stringify` en la página, no con el
  serializador de Playwright: un `Map` o un nodo DOM dan `{}`. Un valor muy
  anidado vuelve como texto en lugar de fallar.

## Observación

La consola y la red se guardan desde la última navegación del marco principal:
200 entradas cada una, texto de consola de hasta 1 KiB, URL de hasta 512
bytes. Un informe devuelve las entradas más recientes que entran en 12 KiB y
cuenta en `omitted` las anteriores. De una solicitud se guarda método, URL,
estado, tipo de recurso y texto de fallo; **nunca cuerpos ni cabeceras**. Los
recursos `data:` no se registran.

Un diálogo JavaScript se descarta y queda registrado en la consola, salvo que
`handle_dialog` haya armado una respuesta para el siguiente. La respuesta se
usa una vez y se olvida si la página navega. El diálogo se contesta desde el
hilo lector: mientras está abierto la página queda bloqueada, y con ella la
solicitud que el driver esté esperando.

## Entrada cruda

Los píxeles de la vista en vivo no pasan por el driver: los entrega quien
embebe ([Chromium embebido](browser-cef.md#cómo-funciona)). La entrada de la
ventana sí.

- **Entrada cruda.** Puntero, rueda, teclas y texto llegan a la página como
  los del usuario. El driver recuerda botones y modificadores apretados: un
  movimiento entre `pointer_down` y `pointer_up` es un arrastre. `MouseButton`
  se serializa como lo nombra el protocolo (`left`, `middle`, `right`), para
  que un host lo lleve en sus propios comandos.
- **Modificadores por evento.** Un host que conoce las teclas modificadoras
  junto con cada evento del puntero, y no cuando bajan y suben, las fija con
  `hold_modifiers(modifiers::SHIFT | modifiers::CONTROL)` antes del evento y
  las quita después con `hold_modifiers(0)`. No genera eventos de tecla: sólo
  cambia el campo `modifiers` de los eventos de puntero y de rueda que siguen.
  Chromium lee de ahí lo que hace con ellos: con Shift un clic extiende la
  selección y, si la rueda no trae movimiento horizontal, la gira de costado;
  con Control la rueda enviada por el protocolo desplaza igual, no amplía.

`screenshot` es otra cosa: la captura para las herramientas, PNG a un píxel de
imagen por píxel CSS cualquiera sea la escala, de hasta 2 MiB.

## Límites

| Dato | Límite |
| --- | --- |
| Árbol accesible | 64 KiB, cortado en línea entera |
| Resultado y script de `evaluate` | 16 KiB cada uno |
| Consola y red | 200 entradas; texto 1 KiB; URL 512 bytes; informe 12 KiB |
| Captura de `screenshot` | PNG de 2 MiB |
| Selección para el portapapeles | 16 KiB |
| Texto de `fill`, `type_text` e `insert_text` | 16 KiB |
| URL | 4096 bytes; sólo HTTP, HTTPS o `about:blank`, sin credenciales |
| Viewport | 320–1600 × 240–1200 píxeles CSS |
| Acciones, esperas y navegaciones | 10 s; adjuntarse a la página, 20 s |
| Mensaje del protocolo | 32 MiB; uno mayor se descarta sin acumularlo y falla sólo su solicitud |
| Marcos de otros procesos y contextos seguidos | 64 |
| Profundidad del árbol accesible | 400 niveles |

El contenido de la página no es confiable. Todo valor que nace en ella se
acota de nuevo en Rust, incluso el que ya acotó el script auxiliar. Un
sustituto UTF-16 suelto en un texto de la página, que no es JSON válido para
una cadena de Rust, se reemplaza en lugar de perder el mensaje.

## Mediciones

Son las del driver frente al bridge, y se conservan como registro: se tomaron
el 11 de octubre de 2026 en esta máquina (16 hilos, 14 GB) sobre el Chrome
headless 150.0.7871.46 que el driver lanzaba entonces, con un ejemplo
`measure` que se quitó con ese motor. **No se repitieron sobre el Chromium
embebido**; lo medido sobre él, a través del runtime, está en
[Chromium embebido](browser-cef.md#mediciones).
En todas las corridas la carga de un minuto estaba entre 0,3 y 1,4 y no había
procesos `rustc`. El bridge se midió con Playwright 1.63.0 sobre el mismo
Chrome y las mismas páginas.

**Memoria** (PSS de `/proc/<pid>/smaps_rollup`, una sesión en `form.html`, dos
corridas de cada uno):

| | Proceso que maneja el navegador | Árbol de Chrome |
| --- | --- | --- |
| Driver Rust | 1,8 MB | 294 MB y 295 MB |
| Bridge Node | 106,9 MB y 106,6 MB | 302 MB y 292 MB |

El árbol de Chrome es el mismo en ambos casos; lo que desaparece son los
~105 MB de Node.

**Latencia**, mediana de 50 repeticiones (p95 entre paréntesis). El bridge
devuelve con cada comando el estado de la página y un PNG del viewport, y con
las acciones también el árbol; la segunda columna compone lo mismo con el
driver para que la comparación sea justa.

| `form.html` (árbol de 2 KB) | Driver, operación sola | Driver, mismo contenido que el bridge | Bridge |
| --- | --- | --- | --- |
| captura accesible | 3,4 ms (4,1) | 33,4 ms (37,1) | 33,5 ms (48,0) |
| clic | 33,4 ms (34,6) | 50,0 ms (52,5) | 66,7 ms (69,8) |
| rellenar | 2,3 ms (3,8) | 33,3 ms (37,1) | 33,5 ms (35,7) |
| evaluar | 0,1 ms (0,3) | 16,7 ms (18,5) | 33,2 ms (37,2) |
| captura PNG | 16,8 ms (31,5) | 16,7 ms (18,3) | 33,7 ms (37,3) |

| `big.html` (1.500 filas, árbol de 64 KiB) | Driver, operación sola | Driver, mismo contenido que el bridge | Bridge |
| --- | --- | --- | --- |
| captura accesible | 464 ms (473) | 483 ms (504) | 555 ms (571) |
| clic | 33,4 ms (34,7) | 519 ms (535) | 585 ms (594) |
| rellenar | 6,2 ms (9,0) | 499 ms (519) | 573 ms (590) |
| evaluar | 0,1 ms (0,2) | 17,0 ms (34,3) | 32,8 ms (37,6) |
| captura PNG | 16,8 ms (17,9) | 17,2 ms (34,6) | 33,1 ms (38,1) |

En la página grande casi todo el tiempo es de Chrome: 345 ms de
`Accessibility.getFullAXTree` para 25.600 nodos (igual con o sin interpretar
la respuesta, y con o sin `Accessibility.enable`) y 83 ms de la captura del
DOM; componer el texto lleva 20 ms. El bridge tarda más en esa página porque
recorre el árbol dos veces.

**Arranque**, hasta tener la página lista: el driver entre 0,33 y 0,38 s y el
bridge entre 0,59 y 0,61 s. El primer arranque después de haber corrido
Chrome con otro entorno tardaba unos 3 s en ambos (2,9 s y 3,2 s medidos).

**Flujo.** La vista de aquel motor era un flujo de cuadros JPEG
(`Page.startScreencast`) a 1280×800 CSS: 60 cuadros por segundo a escala 1 y
38 a escala 2 (2560×1600), con 64 ms de mediana de la rueda al primer cuadro
de una página quieta a escala 2. El límite era el tiempo que tarda un Chrome
headless en rasterizar y capturar 4 megapíxeles.

## Validación

Pruebas unitarias, sin navegador, parte de la suite central (58):

```sh
nix-shell --run "cargo test -p openide-browser --lib"
```

Cubren el emparejamiento de solicitudes, respuestas y eventos, la composición
del texto a partir de árboles grabados, la caducidad de referencias, la
descripción de una referencia frente a texto de la página que la imita, las
teclas y sus combinaciones, el directorio privado y su barrido, y cada
límite.

Las pruebas contra el navegador real están en `agents-window`, que es quien
tiene el motor: `browser_cef_driver_tests.rs` abre una página del Chromium
embebido por escenario y la maneja con la API del driver, sin el runtime.
Ignoradas por defecto, corren con las demás pruebas `real_cef`
([Chromium embebido](browser-cef.md#validación)):

```sh
nix-shell --run "cargo test -p agents-window -- --ignored --test-threads=1 real_cef_driver"
```

Chromium no abre ninguna ventana, no usa la GPU y sólo visita el servidor
HTTP local que inicia cada prueba. Son los escenarios que antes corrían
contra un Chrome lanzado: navegación, texto, portapapeles en dos pasos,
desplazamiento e historial; referencias, acciones, esperas y evaluación, con
el script que no cede; consola, red, diálogos y capturas dentro de sus
límites; capturas accesibles de marcos y de controles propios, y sus
opciones; un marco de otro sitio y una ventana que se abre en la página; la
entrada cruda (hover, selección por arrastre, teclas); y las esperas de
accionabilidad. La muerte del proceso que tiene el navegador se prueba ahora
con la aplicación entera, en `browser_cef_tests.rs`. Lo que era propio de
lanzar un navegador (un ejecutable que falta o termina enseguida, las raíces
temporales de cada lanzamiento) y el flujo de cuadros se fueron con ellos.

Las grabaciones de `tests/recorded` son las respuestas del protocolo para
cada página fixture (`tests/fixtures`) y el texto que dio el navegador. La
suite unitaria reproduce ese texto sin navegador y la prueba real comprueba
que el Chromium embebido siga dando el mismo: las grabaciones, hechas con
Chrome 150, coinciden sin cambios con lo que da Chromium 154. Para regrabarlas
después de cambiar una fixture o de actualizar CEF:

```sh
OPENIDE_BROWSER_RECORD=1 nix-shell --run \
  "cargo test -p agents-window -- --ignored --test-threads=1 real_cef_driver_snapshots_of_the_fixtures"
```

## Lo que no se incluye

Iniciar o buscar un navegador (la página la crea quien embebe), varias
pestañas, descargas, subida de archivos, PDF, trazas, grabación, otros
motores, un transporte TCP o WebSocket, los cuadros de la vista y la forma del
cursor, que el protocolo no informa: las dos últimas las da CEF.

Los eventos del puntero del protocolo sólo llegan a la página y a sus marcos.
La lista abierta de un `<select>` es un widget aparte: las teclas actúan sobre
ella, porque la página se las pasa, y un clic del protocolo encima la cierra.
Por eso la ventana usa ahí las llamadas de CEF.

Atajos conocidos:

- Si `DOMSnapshot.captureSnapshot` falla (una página que supere el límite de
  mensaje), la captura accesible se entrega igual, con referencia en todo
  elemento, sin `[cursor=pointer]` y sin descender a los marcos.

## Referencias

El diseño sigue a [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser)
(Apache-2.0), que reemplazó Node y Playwright por un proceso Rust que habla
CDP: cliente propio y captura a partir del árbol de accesibilidad con
referencias atadas al documento. No se copió código suyo.
La forma del texto de la captura reproduce la de
[Playwright](https://github.com/microsoft/playwright) (Apache-2.0). Ver
[Créditos de terceros](../THIRD_PARTY_NOTICES.md).
