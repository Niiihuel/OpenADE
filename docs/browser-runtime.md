# Navegador del proyecto

La interfaz es nativa de OpenIDE. El contenido web lo procesa Chromium en un
proceso supervisado por Rust, mediante Playwright 1.63.0. El contexto es nuevo
y aislado por conversación: no abre el perfil personal de Chrome. El usuario y
el agente comparten esa misma página, con una cola limitada y las políticas de
permisos del host. No se repiten automáticamente acciones inciertas.

## Arranque y temporales

El proceso Node recibe un entorno reducido, sin claves de proveedores ni
`NODE_OPTIONS`. Rust reemplaza `TMPDIR`, `TMP` y `TEMP` con una carpeta propia,
independiente del shell que inició la aplicación. Esto evita el error
`mkdtemp …/nix-shell-…/playwright-artifacts-… ENOENT` cuando una aplicación
desacoplada sigue viva después de cerrarse `nix-shell`.

Antes de importar Playwright, el bridge crea un subdirectorio privado con modo
0700. En Unix mantiene corta la raíz temporal: si la ruta administrada es
demasiado profunda, usa una carpeta aleatoria `/tmp/oid-XXXXXX`. Chromium crea
sockets Unix por debajo de esa raíz; una ruta válida para archivos puede
superar el límite de `sockaddr_un` y fallar con `Socket path too long`.
El cierre normal o por señal elimina sólo la carpeta creada por ese proceso.
Los instaladores también reciben temporales propios, eliminados al finalizar.
No se recrea un directorio efímero externo que ya fue borrado.

En sistemas habituales se usa el Chromium instalado por Playwright. En NixOS,
si existe, se selecciona el ejecutable empaquetado `chromium`,
`chromium-browser`, `google-chrome-stable` o `google-chrome` del `PATH`: su
wrapper proporciona las bibliotecas que no encuentra el binario Linux genérico
descargado. `OPENIDE_BROWSER_EXECUTABLE` permite elegir explícitamente una ruta.
En ambos casos se mantiene `chromiumSandbox: true` y un perfil temporal nuevo.
El ejecutable debe ser compatible con Playwright; su documentación recomienda
la distribución asociada y advierte que un `executablePath` externo requiere
validación. [BrowserType.launch](https://playwright.dev/docs/api/class-browsertype#browser-type-launch).

El host pasa la escala de la ventana al iniciar el contexto: el
`pixels_per_point` de egui, acotado a 1–3 y redondeado a dos decimales. El
bridge lanza Chromium con `--force-device-scale-factor` y crea el contexto con
el mismo `deviceScaleFactor`; con la emulación sola, los cuadros del screencast
siguen saliendo a 1x. El viewport CSS sigue siendo el tamaño lógico de la
región visible, así que las coordenadas enviadas a la página no cambian. La
escala queda fija mientras viva el contexto: si la ventana pasa a un monitor
con otra, la página conserva la anterior y se muestra reescalada hasta
**Reiniciar navegador**.

Abrir la pestaña no instala nada. **Instalar navegador** instala la biblioteca
y los binarios administrados; no añade bibliotecas al sistema ni modifica el
perfil del usuario. En NixOS hace falta un navegador empaquetado compatible, o
un entorno que proporcione las bibliotecas requeridas por el binario de
Playwright. [Instalación de navegadores](https://playwright.dev/docs/browsers).

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
descuenta estas medidas también antes de recibir la primera captura; mostrar
la terminal conserva la correspondencia entre el área renderizada y las
coordenadas enviadas al navegador.

El contenido tiene estados mutuamente excluyentes: vacío, cargando, página o
error de carga. Un error de navegación reemplaza la captura de Chrome y elimina
su región interactiva. Para `ERR_CONNECTION_REFUSED` en localhost se indica que
hay que verificar el servidor del proyecto y su puerto; la interfaz no inicia
servidores. **Reintentar** vuelve a navegar si el proceso sigue disponible y
recrea el contexto sólo si no pudo iniciarse. Los errores de entrada, como una
copia sin selección, se muestran en el pie y conservan una página válida.

Las referencias de estructura, leídas sin copiar su implementación, son
`references/zcode/packages/ui/src/EmbeddedBrowserPaneParts.tsx` (barra y menú)
y `references/zcode/packages/ui/src/browser-use/BrowserViewportSurface.tsx`
(prioridad de errores y exclusión de la página fallida). OpenIDE conserva su
runtime propio y el contexto aislado por conversación.

El bridge soporta dirección HTTP/HTTPS sin credenciales incrustadas, historial,
recarga, clic, localizadores públicos, texto Unicode, teclas, scroll y resize.
La ventana envía además el puntero: movimiento, botón presionado y botón
soltado, en los mismos píxeles CSS. La página recibe así estados `:hover` y
arrastres (selección de texto, deslizadores); un arrastre que sale de la página
sigue en su borde más cercano y la suelta se entrega siempre.
El host envía ambos ejes del trackpad y convierte Shift + rueda vertical en
desplazamiento horizontal. Pegar y confirmar texto de un método de entrada
requieren que el viewport visible tenga foco; durante una navegación no se
envían al documento nuevo. PageUp y PageDown llegan a la página.
Las URLs `file:`, `javascript:` y similares no forman parte del protocolo.
Cada respuesta a una herramienta contiene la URL, título, historial, captura
PNG y, cuando corresponde, árbol accesible. Las respuestas a la entrada de la
ventana no llevan imagen: su resultado llega por la vista en vivo. Se rechazan
localizadores ambiguos.

La entrada de la ventana tiene una cola propia limitada a 128 comandos y
256 KiB, con hasta cuatro operaciones en vuelo. Agrupa texto Unicode y scroll
adyacentes sin cruzar clics, teclas o navegaciones. De los movimientos del
puntero en cola sólo sobrevive el más nuevo, con la misma regla; uno que no
entra en una cola llena se omite sin aviso. Movimiento y rueda van de a uno en
vuelo, responden sin estado de página y no cuentan como entradas pendientes ni
reconstruyen la ventana. La cantidad pendiente se
muestra en la cabecera; si se alcanza un límite se informa que la nueva entrada no
se envió. Un rechazo anterior al envío conserva las entradas aceptadas para
despacharlas cuando haya espacio. Las acciones del usuario omiten el árbol
accesible; las solicitudes del agente mantienen esa información.

Copiar devuelve la selección al portapapeles nativo sin regenerar PNG ni ARIA.
Cortar usa dos pasos: obtiene texto y un token, y sólo confirma la eliminación
después de entregar la copia al host y validar la conversación, pestaña y foco.
El bridge vuelve a comprobar que el campo, valor y selección no cambiaron.
Admite corte en `input` y `textarea` editables, y copia de selección normal del
documento. Los campos de contraseña no permiten copiar ni cortar; los de sólo
lectura permiten copiar. El corte de `contenteditable` todavía no está
implementado. La selección transferida tiene un máximo de 16 KiB y no se
retiene en el snapshot compartido con herramientas.

Las capturas tienen un máximo de 2 MiB; el protocolo JSONL, 4 MiB por respuesta
y 64 KiB por solicitud; el árbol accesible, 64 KiB. Hay una página por contexto,
sin descargas ni ventanas emergentes. La cola Rust admite 16 comandos; el proxy
MCP acepta hasta cuatro solicitudes simultáneas y sólo contacta su broker local
autenticado. La vista es un flujo de cuadros de Chromium, descrito en
**Vista en vivo**; no es un WebView: no promete audio ni reproducción de video,
formas del cursor, doble clic ni la latencia de un navegador de escritorio.

Si el compositor de Chromium todavía no tiene un frame después de navegar,
se reintenta una vez **la captura**, nunca el clic o la escritura que la causó.
La consulta periódica de estado no borra el error de navegación anterior. La
interfaz muestra una explicación breve con detalle al pasar el puntero; una
acción explícita posterior puede resolverla. La pérdida del proceso exige un
contexto nuevo, para conservar la identidad de las herramientas pendientes.
**Reintentar** conserva el destino HTTP/HTTPS y lo abre una sola vez cuando el
nuevo contexto está listo, incluso si se cambió de pestaña mientras arrancaba.
La barra conserva esa dirección durante `about:blank` y ante una página de
error interna; sólo una navegación confirmada puede sustituirla. No se
reproducen clics, formularios ni escritura de la instancia anterior.

## Vista en vivo

Mientras la superficie está visible, el bridge mantiene un screencast de CDP
(`Page.startScreencast`, JPEG de calidad 80) en una sesión propia. Chromium
entrega un cuadro cada vez que su compositor tiene uno y el bridge lo emite
por stdout como una línea de evento no solicitada, junto a las respuestas:

```json
{"event":"frame","seq":12,"format":"jpeg","scale":2,"data":"<base64>"}
{"event":"stream_lost","error":"…"}
```

`format` es `jpeg` o `png` y `scale` son los píxeles de dispositivo por píxel
CSS del cuadro. Las líneas siguen acotadas a 4 MiB; el base64 que entrega
Chromium se copia a la línea sin recodificar. El host activa y detiene el flujo
con `{"type":"stream","enabled":…}` según `set_visible`: oculto no llega
ningún cuadro. Al volver a mostrarse, la pestaña pinta su último cuadro, pero
la página no recibe entrada hasta el primer cuadro del flujo nuevo. La URL, el título y el historial se consultan cada 300 ms sin
imagen (`{"type":"state"}`) mientras está visible y sin comandos.

**Contrapresión.** El bridge confirma un cuadro a Chromium
(`Page.screencastFrameAck`) recién cuando su línea terminó de drenar. Si el
host lee despacio, Chromium deja de producir cuadros en lugar de acumularlos:
sólo esperan los que caben en el pipe y los que tenía en vuelo. Las respuestas
comparten stdout y quedan, como mucho, detrás de esos cuadros. En el host el
hilo lector separa eventos de respuestas sin bloquearse; cada etapa guarda un
solo cuadro y uno más nuevo reemplaza al que nadie tomó. Un hilo propio
decodifica (JPEG o PNG, con el crate `image`) y el hilo de la interfaz sólo
entrega el resultado a `limn_egui::present_surface`, que escribe en la misma
textura mientras el tamaño no cambie. No hay un `RasterImage`, un SHA-256 ni
una reconstrucción de la ventana por cuadro.

**Nítido en reposo.** 200 ms después del último cuadro, el bridge pide una
captura PNG a escala de dispositivo por la misma sesión y la envía como cuadro
`png`. Una página que sigue animando nunca llega a ese plazo; una que cambia
menos de cinco veces por segundo (un cursor de texto que parpadea) recibe uno
después de cada cambio. Se conserva el JPEG si la página cambió durante la captura, si el PNG no entra en
una línea o si el cuadro está reducido por el presupuesto. Un cuadro idéntico
al anterior no se reenvía: la propia captura provoca uno.

**Presupuesto.** Un cuadro tiene como máximo 3840 × 2160 píxeles (8 294 400).
El viewport más grande, 1600 × 1200, entra completo a escala 2. Por encima
(escala mayor que 2,08 con ese panel) el screencast recibe `maxWidth` y
`maxHeight`, Chromium reduce el cuadro y el host lo muestra en su tamaño
lógico, reescalado. Un JPEG que no entra en una línea se descarta y el flujo se
reinicia con 20 puntos menos de calidad, hasta 20.

**Superficie.** La página se pinta con `Element::surface` de limn: el cuadro se
ancla a la esquina superior izquierda de la región, alineado a la grilla de
píxeles, y cada píxel suyo ocupa un píxel físico. Un cuadro que difiere de la
región por un píxel de redondeo se recorta o deja un borde; no se estira. Un
punto es un píxel CSS, también mientras Chromium alcanza un cambio de tamaño.

**Capturas del agente.** Las herramientas y el proxy MCP reciben la misma
captura PNG a 1x (`scale: 'css'`) y el mismo árbol accesible, con los mismos
límites. Con una escala distinta de 1 esa captura hace que Chromium dibuje la
página a otra escala por un instante: el bridge no envía los cuadros de ese
momento y, si pudieron ocultar un cambio real, pide el cuadro sin pérdida. Toda
captura a escala CSS, del viewport, de la página completa o de un elemento, se
toma dentro de `atCssScale` en el bridge para que eso valga también para ella.

**Pérdida del flujo.** Si falla la confirmación de un cuadro, la captura sin
pérdida o la sonda que acompaña a `state`, o si el proceso de la página
termina, el bridge emite `stream_lost`. El host conserva la página, lo dice en el pie
(«La vista en vivo se detuvo…») y vuelve a pedir el flujo cada 2 s.
**Actualizar vista** lo reinicia a pedido.

**Mediciones.** Con el bridge de producción el 11 de octubre de 2026, sin la
ventana: Google Chrome 150 a escala 2 y un viewport de 1280 × 800 (cuadros de
2560 × 1600), sin otras compilaciones en curso en el equipo:

| Medida | Resultado |
| --- | --- |
| Cuadros por segundo, página animando | 34–38 (JPEG de unos 308 KiB en base64) |
| De la rueda al primer cuadro, página animando | mediana 19–22 ms, p90 24–27 ms |
| De la rueda al primer cuadro, página quieta | mediana 57–63 ms, p90 59–67 ms |
| Respuesta a la rueda de la ventana | mediana 3 ms |
| Ida y vuelta de un movimiento del puntero | 16,6 ms (quieta), 29 ms (animando) |
| Respuesta a un comando durante el flujo | mediana 3–6 ms, máximo 7 ms |
| Cuadro sin pérdida | 383 KiB en base64, 239–256 ms después del último cuadro |
| Cuadros decodificados por segundo en el actor Rust, página animando | 37, ninguno reemplazado antes de decodificar |
| Decodificación en el host, fuera del hilo de la interfaz | JPEG: mediana 9,2 ms, p90 9,7 ms; PNG: mediana 5,2 ms, p90 6,3 ms |
| Entrega a la textura en el hilo de la interfaz | unos 5 µs por cuadro |

Con la página quieta, Chromium solo, sin el bridge, tarda 54–65 ms de la rueda
al cuadro a esta escala y 22 ms a escala 1: es el costo de rasterizar y
codificar 4 megapíxeles, no del transporte. A escala 1 el mismo viewport da 60
cuadros por segundo. Con otras compilaciones en curso en el mismo equipo se
midieron 11–26 cuadros por segundo. La copia del cuadro a la GPU dentro del
renderizador (16 MiB por cuadro) no se midió, y nadie verificó todavía estas
propiedades mirando la ventana real.

## Chromium y Servo

Se conserva Chromium para esta implementación. Ya cubre las APIs públicas de
Playwright empleadas por el bridge y el historial obtenido mediante CDP. Servo
no es un reemplazo de `chromium.launch`: la integración tendría que implementar
otro adaptador de entrada, capturas, accesibilidad y automatización. Esta es una
conclusión sobre la arquitectura actual, no una medición comparativa de memoria
o rendimiento. [Motores soportados por Playwright](https://playwright.dev/docs/browsers).

Servo sí ofrece una API de integración Rust real. Su documentación describe
`ServoBuilder`, `WebView`, un `RenderingContext`, notificación de nuevos frames,
envío de eventos y ejecución de `spin_event_loop`. Puede ser una alternativa
para una futura superficie web integrada; requiere conectar ese ciclo y su
renderizado con el host. [API oficial de Servo](https://doc.servo.org/servo/index.html).
El proyecto también documenta versiones mensuales con cambios de API durante
esta etapa. [Política de compatibilidad de Servo](https://servo.org/blog/2026/05/31/april-in-servo/).

## Validación reproducible

```sh
rtk env OPENIDE_BROWSER_E2E_ARTIFACTS=/tmp/openide-browser-e2e-review \
  node --test scripts/browser-e2e.mjs
```

`OPENIDE_PLAYWRIGHT_MODULE_DIR` puede apuntar a una instalación existente de
Playwright. El valor predeterminado es el directorio administrado de OpenIDE.
La prueba no instala dependencias y sólo visita un servidor HTTP fixture local.
Usa el bridge de producción y procesos Node reales; verifica temporal eliminado,
ruta temporal larga, PNG, Unicode, selección con teclado, localizadores, scroll,
historial, resize, error de navegación y recuperación, dependencia ausente y
reintento, limpieza de perfiles y transporte del proxy MCP. Para la vista en
vivo comprueba, a escala 1 y 2: cuadros mientras la página anima, tamaño igual
a viewport × escala, ninguno oculta, el cuadro sin pérdida al quedar quieta y
sin repetirse, respuestas a comandos durante el flujo, un lector detenido que
no acumula cuadros, capturas del agente a 1x que no llegan a la vista, estado
`:hover`, selección por arrastre y cuadros al nuevo tamaño tras un resize. Otro
caso mide el flujo a 2560 × 1600 y lo informa como diagnóstico. También comprueba
copia, corte en dos fases, rechazo si el campo cambia, campos de sólo lectura,
contraseñas, `contenteditable` y límites de texto. El broker del
último caso es una fixture local; no pretende probar el catálogo de permisos
Rust ni los eventos de la ventana nativa.

Validado el 11 de octubre de 2026 con Playwright 1.63.0 y Google Chrome
150.0.7871.46 empaquetado por NixOS, con sandbox habilitado: siete casos reales
pasaron. Las pruebas Rust de entorno, limpieza, límites y conservación del
error están en `browser_runtime_tests.rs`, junto a las del flujo: eventos que
no entran al canal de respuestas, reemplazo del cuadro que nadie tomó,
formato y presupuesto de píxeles al decodificar, límites del puntero y de la
escala, y el aviso de flujo perdido; las compila la suite central.
`browser_host_tests.rs` verifica además el foco, las entradas, el destino de
reintento entre conversaciones, la conservación de ráfagas de entrada y el
portapapeles entre conversaciones con un actor de prueba sin Node, navegador,
sockets ni acceso al entorno personal, y que el puntero llegue de a un
movimiento en vuelo y que un arrastre termine donde se suelta.
`browser_input_app.rs` contiene las regresiones de orden, agrupación y rechazo
antes de modificar una cola llena. El mapeo 1:1 de la superficie a escala 1,
1,5 y 2 y la reutilización de su textura se prueban en limn (`limn-egui`).

Dos pruebas Rust opt-in recorren el camino real sin la GPU: el actor, su
lector y su decodificador contra el bridge y Chromium reales con una página
servida en localhost, y el costo por cuadro sobre los cuadros que guarda la
prueba del bridge. Se ejecutan con el perfil optimizado:

```sh
rtk env OPENIDE_PLAYWRIGHT_MODULE_DIR=<deps> OPENIDE_BROWSER_FRAME_SAMPLES=/tmp/openide-browser-e2e-review \
  cargo test --profile desktop -p agents-window -- --ignored --nocapture \
  frame_cost_on_the_decoder the_actor_streams
```

`browser_ui_tests.rs` cubre la barra a 240–900 px, estados de error sin captura,
acciones del menú y geometría estable del viewport. La revisión visual opt-in
con fuentes nativas exporta vacío, carga, conexión rechazada, página, menú y
fallo de arranque a 320 y 640 px, sin lanzar procesos ni servidores:

```sh
rtk cargo test -p agents-window browser_ui::visual_tests::export_browser_ui_visual_review -- --ignored
```

Los PNG/SVG se escriben en `/tmp/openide-browser-ui-review`. El contenido de la
página es una fixture visual identificada; la prueba real del bridge anterior
es la que valida la comunicación con Chromium.

La prueba `scripts/browser-desktop-e2e.py` cubre el recorrido completo desde la
ventana Wayland real; su última ejecución es anterior a la vista en vivo y no
se repitió con ella: arranque con `TMPDIR` eliminado, navegación al
servidor HTTP local, clic de foco, ráfaga de texto Unicode, copia al portapapeles
del sistema, corte confirmado y pegado íntegro. Pasaron sus cinco comprobaciones.
Requiere un build con `--features editor-e2e`, Hyprland, `wtype`, `dotool`,
`grim` y `wl-paste`. Usa un directorio marcado y un workspace aislados; verifica
el PID antes de enviar entrada. La feature y los argumentos de fixture no se
incluyen en el build normal.

```sh
rtk cargo build --profile desktop -p agents-window --features editor-e2e
rtk python3 scripts/browser-desktop-e2e.py --binary target/desktop/agents-window \
  --output /tmp/openade-browser-native-e2e
```
