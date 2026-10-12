# Chromium embebido (CEF)

Con la opción de compilación `cef`, el navegador del proyecto corre sobre un
Chromium embebido en la propia aplicación
([CEF](https://bitbucket.org/chromiumembedded/cef) 154.0.34, a través de
[cef-rs](https://github.com/tauri-apps/cef-rs)) en lugar de lanzar un Chrome
headless. Está apagada por defecto. El resto del navegador no cambia: los
mismos comandos, el mismo [driver](browser-driver.md), las mismas
[herramientas](browser-tools.md) y la misma vista.

Es la primera mitad del paso 4 del
[plan](browser-runtime.md#motor-chromium-servo-y-lo-que-sigue): todo lo que no
necesita GPU ni ventana. Chromium compone la página en la CPU y entrega cada
cuadro sin comprimir, a la escala de la ventana, diciendo qué parte cambió:
la aplicación copia, convierte y sube a su textura sólo esa parte. La segunda
mitad, cuadros compartidos por GPU, no está hecha.

**Nadie vio todavía un cuadro de esto en la ventana real.** Todo lo que sigue
se comprobó sin ventana: con pruebas, con mediciones a través del runtime y
mirando volcados PNG de los cuadros que recibiría la vista. Eso incluye las
listas desplegables, el cursor y el aviso del pie: que la ventana ponga de
verdad el cursor que la página pide, y que la lista se vea bien compuesta en
pantalla, falta mirarlo. También incluye las partes de cuadro: que la
textura de la ventana quede igual a la página se comprobó con las escrituras
que egui le entrega al renderizador, no con la GPU, y falta mirar en pantalla
que no quede ningún rectángulo viejo después de desplazar, de abrir y cerrar
una lista o de cambiar el tamaño del panel.

## Compilar y abrir

```sh
nix-shell nix/shell-cef.nix --run "cargo build --profile desktop -p agents-window --features cef"
nix-shell nix/shell-cef.nix --run "target/desktop/agents-window"
```

`nix/shell-cef.nix` es el shell normal más CEF (`nix/cef.nix`, un archivo de
300 MB que el shell normal nunca descarga). Define `CEF_PATH`, con lo que
`cef-dll-sys` enlaza contra ese directorio y no descarga nada, y lo agrega al
final de `LD_LIBRARY_PATH`: de ahí se carga `libcef.so`, y CEF lee sus
recursos y sus locales del directorio del que se cargó la biblioteca. Va al
final porque ese directorio trae también el cargador de Vulkan de Chromium
con el nombre del del sistema (`libvulkan.so.1`), y el de la ventana de la
aplicación tiene que seguir siendo el del sistema.

El script de compilación de cef-rs copia además los archivos de CEF (unos
360 MB) a `target/<perfil>/`, y cargo pone ese directorio primero en la ruta
de bibliotecas de todo lo que ejecuta (`cargo run`, `cargo test`). Por eso
`crates/openide-cef/build.rs` borra de ahí la copia de `libvulkan.so.1` cada
vez que cef-rs la deja. El resto de la copia es lo que usan las pruebas; al
abrir el binario directamente, como hace `scripts/build.py`, no se usa.

Para comprobar una instalación sin abrir la ventana:

```sh
nix-shell nix/shell-cef.nix --run "target/desktop/agents-window --cef-probe"
```

Inicia un contexto como lo hace la ventana, espera su primer cuadro y lo dice:
«CEF funciona: primer cuadro de 2560 × 1600 a los 160 ms, sin ventana y sin
GPU.» Si CEF no arranca dice por qué y termina con código 1.

Sin la opción nada de esto se compila ni se enlaza. `crates/openide-cef` está
excluido del workspace, el árbol de dependencias por defecto es el mismo
(`cargo tree`) y el binario no nombra `libcef.so`. Lo único que cambia para una
compilación normal es `Cargo.lock`, que ahora nombra los paquetes de cef-rs: en
una máquina que nunca los vio, cargo clona ese repositorio (39 MB de fuentes,
no CEF) para resolver el archivo, aunque no compile nada de él.

## Cómo funciona

| Pieza | Qué hace |
| --- | --- |
| `crates/openide-cef` | Inicia CEF una vez por proceso, abre páginas sin ventana y entrega de cada una sus cuadros y su extremo DevTools. Es lo único que enlaza `libcef.so`. |
| `Driver::connect_to_page` | El driver se adjunta a una página que ya existe en lugar de crear su contexto y su página. |
| `apps/agents-window/src/browser_cef.rs` | El contexto embebido del runtime: abre la página, guarda su último cuadro y convierte lo que cambió en la parte que toma la vista. |

**Procesos.** CEF inicia sus procesos auxiliares (zygotes, renderers, red,
compositor) con el mismo ejecutable de la aplicación. `main` los atiende antes
que cualquier otra cosa, junto al modo `--browser-mcp`: si el proceso se inició
con `--type=…` corre como auxiliar y termina. Cualquier otro inicio sigue de
largo sin llamar a CEF. Sin la opción esa línea no existe. El sandbox de
Chromium queda activo (los renderers corren con seccomp).

**Arranque.** CEF no se inicia con la aplicación sino con el primer contexto
de navegador, en un hilo propio (`openide-cef`) que también lo cierra: CEF
exige que lo cierre el hilo que lo inició. Su bucle de mensajes corre en un
hilo de CEF, nunca en el de la interfaz. Los archivos de CEF van a un
directorio privado `oid-XXXXXX` como el de Chrome, con las mismas reglas
(modo 0700, ruta corta, recogido por el siguiente arranque si el proceso
murió). CEF se inicia una sola vez por proceso: si falla, no se reintenta.

**Sin GPU y sin pantalla.** Chromium corre con `--disable-gpu`,
`--disable-gpu-compositing` y `--disable-software-rasterizer`: sin aceleración,
sin componer en GPU y sin GL por software en su lugar, así que WebGL no está
disponible. Corre además con `--ozone-platform=headless`: no se conecta a
Wayland ni a X11 y no abre ningún dispositivo DRM. Con la plataforma Wayland
el proceso de la aplicación y el del compositor abrían el nodo de render de la
GPU aunque no cargaran ningún driver; con la plataforma headless no hay nada.
Una prueba lo vigila: ningún proceso de CEF tiene abierto `/dev/dri/*` ni
`/dev/nvidia*` ni mapea un driver gráfico. Sigue existiendo un proceso que
Chromium llama «de GPU»: ahí corre su compositor por software. Ese proceso
mapea el cargador de Vulkan que trae CEF (`libvulkan.so.1`) y ningún driver;
`libdrm` y `libgbm` aparecen en todos porque `libcef.so` los enlaza.

**Un navegador por contexto.** Cada conversación tiene su propio navegador de
CEF con un contexto de solicitudes propio y sin ruta de caché: un perfil en
memoria que nadie más usa. Cookies, caché HTTP, `localStorage`,
`sessionStorage` e IndexedDB no se ven desde otro contexto y desaparecen al
cerrarlo. Nada de eso llega al disco.

**Driver.** `openide-cef` entrega un `Transport` sobre la API de mensajes
DevTools de CEF: enviar es `SendDevToolsMessage` y recibir es un observador
que pasa cada mensaje crudo a un canal de 256 lugares. No hay puerto ni
tubería. El extremo de CEF es la página y no el navegador, así que
`connect_to_page` le pregunta quién es (`Target.getTargetInfo`), se adjunta a
ella con una sesión propia y sigue igual que siempre. No crea contexto ni
página, no pide descubrir otros destinos (serían las páginas de otras
conversaciones) y no toma la captura de un píxel que necesita Chrome headless.
Herramientas, capturas, referencias, consola, red, diálogos, marcos de otro
sitio y entrada de la ventana pasan por el mismo driver.

Lo que con Chrome resuelve el driver por el protocolo del navegador, acá lo
resuelve quien embebe, sin mostrar nunca una ventana:

- Una ventana emergente no llega a crearse; `window.open` devuelve `null`.
  Lo que un clic pidió ver en otra pestaña se abre en la misma página: ver
  **Enlaces que piden otra pestaña**, más abajo.
- Un diálogo JavaScript no se muestra. El driver lo contesta por DevTools
  como siempre; CEF sólo lo deja abierto hasta entonces.
- El selector de archivos se cancela, el menú contextual queda vacío y las
  descargas se rechazan.

**Escala.** En la plataforma headless CEF respeta la escala que declara la
vista: una vista de 1280 × 800 a escala 2 se pinta en 2560 × 1600 con
`devicePixelRatio` 2. La escala se fija al crear la página. Se acota una vez
a 2,07, la que deja entrar el viewport más grande (1600 × 1200) en el
presupuesto de 3840 × 2160 píxeles por cuadro.

**Cuadros.** CEF llama a `on_paint` en su hilo de interfaz con el búfer BGRA
de toda la vista y el rectángulo que Chromium volvió a pintar. Ahí sólo se
copia ese rectángulo a un lienzo, que queda igual al búfer de Chromium, y se
despierta al hilo de cuadros del runtime. Ese hilo convierte a RGB lo que se
pintó desde su turno anterior, como un solo rectángulo, y arma con eso una
parte del cuadro que la vista ya tiene (`SurfaceFrame::within` de limn). La
ventana la sube a su textura con `present_surface`, que escribe sólo ese
rectángulo. Un cambio cuesta lo que ocupa: el cursor de texto que parpadea
son unas decenas de píxeles dos veces por segundo, y una página quieta no
cuesta nada. La entrega a la vista es la de siempre, `LiveView::publish`, en
un solo lugar. No hay JPEG ni PNG ni `screencast`. Las respuestas al agente
comparten hilo con la copia y esperan a lo sumo una.

- **Cuándo va el cuadro entero.** En la primera pintura, cuando la página
  cambia de tamaño, cuando la vista vuelve a mostrarse, con **Actualizar
  vista** y cuando lo pintado cubre toda la vista (2,9 ms de copia y 3,4 ms
  de conversión a 2560 × 1600, como antes).
- **Nada se pierde y nada se encola.** Hay un solo lienzo y, esperando a la
  ventana, a lo sumo un cuadro entero y la parte que cambió después. Una
  parte que la ventana no llegó a tomar se reemplaza por la siguiente, que
  abarca también su rectángulo: la vista puede saltear un cuadro, pero no
  pierde lo que ese cuadro había pintado.
- **Un rectángulo por vez.** Chromium informa un rectángulo por pintura y acá
  se junta uno por cuadro: dos cambios chicos en puntas opuestas de la página
  cuestan el rectángulo que los contiene a los dos.
- **El orden de los píxeles.** Chromium entrega BGRA y las texturas de egui
  sólo reciben RGBA premultiplicado (`Color32`); no hay forma de pasarle el
  búfer tal cual. Se convierte sólo lo que cambió.
- **Si la superficie ya no tiene ese cuadro.** limn rechaza una parte que no
  es del cuadro que la superficie muestra (otro tamaño, otra escala o una
  textura que ya no existe) y la ventana pide entonces un cuadro entero, que
  siempre reemplaza todo.
- **Pedir toda la vista** (al mostrarla o con **Actualizar vista**) hace que
  CEF entregue el búfer en el momento, aunque Chromium esté componiendo en él
  el cuadro siguiente: ese cuadro entero puede salir mezclado con el que
  viene. La pintura del cuadro siguiente llega enseguida con su rectángulo y
  lo corrige (visto en las pruebas: el búfer cambió mientras se lo leía; que
  la causa sea ésa es inferido).

- **Oculta.** La página no se oculta para CEF: una página oculta deja de
  animar y de avanzar para el agente. Se descartan sus cuadros, se suelta el
  lienzo y Chromium compone a 10 cuadros por segundo en lugar de 60. Mientras
  corre un comando vuelve a 60, porque un clic o una captura esperan uno o
  dos cuadros.
- **Al mostrarse** se pide un pintado completo: la vista recibe un cuadro
  aunque la página esté quieta.
- **Capturas del agente.** Chromium vuelve a pintar la página en cada captura
  y, si es de un elemento o de la página completa, la dibuja distinta por un
  momento. Las pinturas de ese momento se retienen igual que con el flujo
  (durante la captura y 400 ms después): el lienzo las recibe y la vista no.
  Lo que queda sólo se muestra si difiere de lo que la vista ya tenía, y va
  como una sola parte con todo lo que se pintó mientras tanto.
- **Cambio de tamaño.** Primero cambia la vista y después la página; puede
  verse un cuadro intermedio.

**Entrada.** Sigue pasando por los eventos del protocolo del driver, con una
excepción: el puntero sobre una lista desplegable, que se describe abajo.
Medido contra la llamada nativa de CEF para la rueda: con la página quieta,
del evento al primer cuadro pasan 15,4–17,6 ms de mediana por el driver y
15,3–17,1 ms por CEF. Desplazando, la llamada nativa da 8,0–9,0 ms hasta el
cuadro siguiente contra 16,6–16,7 ms, pero mide otra cosa: Chromium anima el
desplazamiento de la rueda nativa, así que siempre hay un cuadro en camino,
muestre o no ese evento. No hay una ventaja medida que justifique mover toda
la entrada a las llamadas nativas. Un clic enviado en el mismo instante en
que termina una carga se pierde, y 20 ms después ya llega (medido). Se
atribuye a que Chromium descarta la entrada sobre un documento que todavía no
pintó nada; eso no se comprobó.

**Listas desplegables.** La lista de un `<select>` no es parte de la página:
Chromium la pinta aparte y la entrega por otros avisos (se muestra o se
oculta, dónde va en la vista, sus píxeles). `openide-cef` los pasa a la misma
función que recibe los cuadros, con la esquina de la lista ya en píxeles de
dispositivo, y `browser_cef.rs` guarda esos píxeles al lado del lienzo de la
página y los dibuja encima, respetando su transparencia, en la parte del
cuadro donde caigan. Una lista que se abre, cambia (la fila bajo el puntero),
se mueve o se cierra marca como pintado el rectángulo que ocupaba y el que
ocupa: la vista recibe la lista o, donde ya no está, la página que había
debajo, sin que Chromium vuelva a pintarla. El selector de fecha y los demás
widgets que Chromium abre así pasan por el mismo camino; **sólo se probó la
lista de un `<select>`**.

- **Teclado.** Flechas, Enter y Escape van por el driver como siempre: la
  página se los pasa a su lista.
- **Puntero y rueda.** Los eventos del puntero del protocolo sólo llegan a la
  página, que cierra la lista al recibir un clic. Mientras hay una lista
  abierta y el puntero de la ventana está sobre ella, el movimiento, el botón
  izquierdo y la rueda van por las llamadas de CEF
  (`SendMouseMoveEvent`, `SendMouseClickEvent`, `SendMouseWheelEvent`), que sí
  la alcanzan: la fila bajo el puntero se resalta, un clic la elige y la rueda
  recorre la lista sin mover la página. Fuera de la lista todo sigue por el
  driver. Cuando el botón se suelta sobre la lista también se le avisa a la
  página, que pudo haberlo visto bajar sobre el `<select>`.
- **Vista oculta.** Al ocultarse la vista suelta los píxeles de la lista junto
  con los de la página; al volver se piden otra vez los dos.

**Enlaces que piden otra pestaña.** Un panel tiene una sola página, y nunca
se crea otra ventana ni otra pestaña. Lo que un clic pidió ver en una se abre
en esa misma página, y **Atrás** vuelve a la anterior:

| Lo que hace la página o el usuario | Qué pasa |
| --- | --- |
| Clic en un enlace con `target="_blank"` o con el nombre de otra ventana | El driver abre esa dirección en la misma página. |
| Clic en algo cuyo script llama a `window.open(dirección)` | Lo mismo. El script recibe `null`, como con una ventana bloqueada. |
| Control + clic, Shift + clic o botón del medio sobre un enlace | Chromium lo abre en la misma página. |
| `window.open`, un clic hecho por script o un Control + clic hecho por script, sin un clic del usuario detrás | No abre nada. |
| Un destino que no es `http` ni `https` (`data:`, `about:blank`, `javascript:`, `file:`) | No abre nada. |
| Un formulario que envía a otra pestaña (`target="_blank"`) | Se abre su dirección, pero **sin lo que el formulario enviaba**: es una carga nueva por GET. |

Los dos primeros casos los resuelve el [driver](browser-driver.md): la página
anuncia por el protocolo que va a abrir una ventana, con la dirección y con
si hubo un clic del usuario (`Page.windowOpen`), CEF rechaza la ventana y el
driver navega la página a esa dirección. Las herramientas del agente siguen
la misma regla, porque su clic es un clic: `browser_click` sobre un enlace
así espera la página nueva y responde con su dirección, su título y su
árbol, igual que con un enlace común. La dirección se pide una sola vez
(contado en la prueba). La página que se abre no conoce a la que la abrió
(`window.opener`), y el driver no le pasa `Referer` (leído en el código, no
medido). Una ventana que se abre vacía para que el script la llene después
(`window.open()` y más tarde `ventana.location = …`) no abre nada.

**Cursor.** Chromium avisa qué cursor pide la página bajo el puntero
(`OnCursorChange`) y la ventana lo muestra mientras el puntero está sobre la
página y ésta recibe entrada: mano sobre un enlace, barra de texto sobre un
texto o un campo, flechas de redimensionar, prohibido, mover, agarrar, ayuda,
espera, zoom y los demás cursores de CSS que egui tiene. Al salir de la página
vuelve la flecha. Un cursor con imagen propia (`cursor: url(…)`) se muestra
como flecha.

**Salida.** Al terminar el proceso por cualquier salida normal se cierran las
páginas que queden, se cierra CEF y se borra su directorio. Si el proceso
muere sin eso (probado con SIGTERM, SIGHUP y SIGKILL), los auxiliares terminan
solos al perder a su padre y el directorio lo recoge el siguiente arranque.
Las señales terminan la aplicación igual que sin CEF.

## Si CEF no arranca

El contexto se inicia con el Chrome del sistema, igual que sin la opción. El
pie del panel lo dice en su propia línea, «El navegador embebido no pudo
iniciarse; se usa el del sistema.», con el motivo al pasar el puntero y
**Descartar** a la derecha. Aparece cuando el pie no tiene otra cosa que decir
(un error, entradas pendientes o una carga van primero) y no cambia el tamaño
de la página. Descartado, no vuelve a aparecer mientras la aplicación siga
abierta, tampoco en otra pestaña de navegador. El motivo queda además una vez
en la salida de error: «OpenIDE: el navegador embebido (CEF) no pudo iniciarse
y se usa el navegador del sistema. …». Si tampoco hay un Chrome instalado se
ve el panel de siempre, «No se encontró un navegador».

Probado con el binario de la aplicación: recursos de CEF ausentes junto a
`libcef.so`. Se comprueba antes de iniciarlo, porque CEF aborta el proceso
entero si faltan. Probado sólo con `openide-cef`, fuera de la aplicación:
`TMPDIR` apuntando a un directorio que ya no existe. CEF falla solo, con
código 21 y «Failed to create socket directory», y el proceso sigue.

Lo que no tiene salida:

- Sin `libcef.so` en la ruta de bibliotecas la aplicación no arranca: se
  enlaza al cargar el proceso.
- Donde el sandbox de Chromium no puede usarse (un sistema que restringe los
  espacios de nombres de usuario) Chromium aborta el proceso. **No se probó**;
  en esta máquina funciona.

Un Chrome elegido de forma explícita por quien inicia el contexto (las pruebas
lo hacen) se usa en lugar de CEF. `OPENIDE_BROWSER_EXECUTABLE` sólo elige el
Chrome de reemplazo.

## Lo que no salió como se suponía

- **La emulación no da escala 2.** La prueba de viabilidad daba por buena una
  vista en píxeles físicos más `Emulation.setDeviceMetricsOverride`. El búfer
  mide 2560 × 1600 y la página dice `devicePixelRatio` 2, pero Chromium la
  pinta a escala 1 en un cuarto del búfer: se vio en el volcado del cuadro.
  Aquella prueba había mirado una captura DevTools, que sale de otra
  superficie. Observado con la plataforma headless; bajo Wayland no se volvió
  a mirar. Con esa emulación los rectángulos sucios también llegan divididos
  por la escala.
- **La plataforma headless sí respeta la escala**, y con el GPU apagado sí
  pinta (con GPU, la prueba de viabilidad no recibía ningún cuadro).
- **CEF puede iniciarse desde un hilo que no es el principal**, que es lo que
  permite iniciarlo recién con el primer contexto.
- **Soltar un objeto de CEF puede reentrar.** Liberar la última referencia a
  la respuesta de un diálogo vuelve a llamar al manejador de diálogos en el
  mismo hilo. Nada de CEF se suelta con un candado tomado.
- **Faltando sus recursos, CEF aborta el proceso** en lugar de fallar.
- **CEF trae un `libvulkan.so.1` propio** y cef-rs lo copia junto a los
  ejecutables. Con el directorio de CEF primero en la ruta de bibliotecas, o
  al usar `cargo run` después de compilar con la opción, la ventana de la
  aplicación habría abierto ese cargador en lugar del del sistema. No llegó a
  pasar: nada de esto se ejecutó con ventana.
- **Chromium toma SIGHUP, SIGINT y SIGTERM** aunque se le pida que no instale
  manejadores (`disable_signal_handlers` sólo conserva los de fallos): con
  ellos, una señal terminaba la aplicación con código 0. Se vuelven a dejar
  como estaban apenas CEF termina de iniciar.

## Límites

- Sin WebGL y sin H.264, HEVC ni AAC (el CEF precompilado no los trae).
- La página se compone a la frecuencia con que la ventana se presenta, que
  con vsync es la de la pantalla: la ventana mide sus primeros 48 cuadros con
  un navegador abierto (`observe_window_frame` en `browser_cef.rs`) y hasta
  entonces son 60. Se mide una vez por ejecución: una ventana movida a una
  pantalla de otra frecuencia conserva la primera. A 120, sin ventana, la
  animación de prueba (una esquina que gira) llega a 120,3–120,4 con 7–8 %
  de un núcleo en la aplicación y 19–20 % en los procesos de Chromium; una
  que cambia toda la vista, con 80–81 % y 23–24 % (ver
  [Mediciones](#mediciones)). Con ventana, en una pantalla de 120 Hz y la ventana dibujada por
  la gráfica integrada, un panel de 1154 × 1474 píxeles sostuvo 120 cuadros
  por segundo de la aplicación y 120 pinturas de Chromium durante 32 s. Los
  dos relojes son independientes: cada ~15 s, durante 2–3 s, la vista toma
  90–110 cuadros distintos de los 120 pintados (dos pinturas caen en un mismo
  refresco y ninguna en el siguiente). Atarlos pide que la ventana marque los
  cuadros de Chromium.
- Lo que cambia en toda la vista (un desplazamiento, un video a pantalla
  completa) sigue costando un cuadro entero por pintura: la copia y la
  conversión no bajaron para ese caso, y son casi todo lo que gasta la
  aplicación. Lo que queda por sacar ahí es la pasada intermedia a RGB (el
  lienzo es BGRA y la textura RGBA), que pide que limn acepte BGRA.
- Lo pintado se junta en un solo rectángulo por cuadro: dos animaciones
  chicas y alejadas cuestan el rectángulo que las contiene.
- Una lista desplegable abierta no sale en las capturas del agente ni en su
  árbol: sólo la ve quien mira la ventana. `browser_select_option` funciona.
  Queda abierta si se clica en otra parte de la aplicación, fuera de la
  página, hasta el próximo clic dentro de ella (leído en el código: la página
  nunca pierde el foco; **no se miró en la ventana**).
- La página no se amplía: Control + rueda no hace nada
  ([por qué](browser-runtime.md#interacción-errores-y-límites)).
- Un formulario que envía a otra pestaña abre su dirección sin los datos, y
  una ventana que un script abre vacía para llenarla después no abre nada:
  ver **Enlaces que piden otra pestaña**.
- El proceso enlaza `libcef.so` al arrancar: unos 50 MB más de PSS aunque no
  se abra ningún navegador, también en cada proceso `--browser-mcp`.
- Los auxiliares de CEF heredan el entorno de la aplicación; Chrome recibía
  uno reducido. Las páginas no pueden leerlo.
- CEF escribe sus propias advertencias en la salida de error. Los mensajes de
  consola de las páginas no aparecen ahí. Después de un diálogo JavaScript,
  cada navegación deja una línea «Default dialog implementation requires a
  parent window handle»: es CEF cancelando un diálogo que ya no existe.
- Un fallo fatal de Chromium en su parte de navegador termina la aplicación,
  porque esa parte corre dentro de ella.
- `navigator.webdriver` es `false`. El Chrome lanzado lleva
  `--enable-automation`, que lo pone en `true` (no se comparó).
- Sólo Linux. No se compiló para otro sistema.

## Mediciones

Tomadas el 11 de octubre de 2026 a través del runtime real, sin ventana, con
las pruebas `real_cef_measurements` y `real_chrome_measurements` compiladas
con el perfil `desktop`: viewport de 1280 × 800 a escala 2 (cuadros de
2560 × 1600), carga de un minuto por debajo de 1 y ningún `rustc` en curso.
Cada cifra es el rango de ocho corridas con CEF y de tres con Chrome
150.0.7871.46, cada una en un proceso nuevo. Los tiempos hasta un cuadro son
hasta el cuadro listo para la ventana.

| Medida | CEF, por software | Chrome headless |
| --- | --- | --- |
| Cuadros por segundo, página animando | 59,8 a 60,1 | 35,6 a 35,9 |
| Cuadros por segundo, desplazamiento continuo | 59,6 a 59,7 | 30,5 a 31,5 |
| De la rueda al siguiente cuadro, desplazando | mediana 16,6–16,7 ms, p90 17,0–17,1 | mediana 31,0–31,7 ms, p90 48–62 |
| De la rueda al primer cuadro, página quieta | mediana 15,4–17,6 ms, p90 16,1–19,0 | mediana 63,0–63,7 ms, p90 64,6–65,6 |
| Procesador mientras anima una esquina: la aplicación | 5 a 6 % de un núcleo (37 a 48 % antes de enviar sólo lo que cambia) | 37 a 38 % |
| Procesador mientras anima una esquina: procesos del navegador | 10 a 11 % | 240 a 241 % |
| Procesador desplazando: aplicación y navegador | 42–43 % y 52–53 % (antes 40–52 % y 51–52 %) | 36–37 % y 234–238 % |
| Procesador animando fuera de pantalla | 1–2 % y 1–2 % | 1 % y 8 % |
| Memoria que suma la aplicación, vista oculta | 82,6 a 86,3 MB | 2,8 a 3,2 MB |
| Memoria que suma la aplicación, página quieta a la vista | 149,4 a 153,1 MB | 61,3 a 61,5 MB |
| Memoria de los procesos del navegador, vista oculta | 220 a 222 MB en 9 | 318 a 327 MB en 11 |
| Memoria de los procesos del navegador, animando | 214 a 215 MB | 400 a 413 MB |
| Total con la vista oculta | 303 a 308 MB | 321 a 330 MB |
| Total animando a la vista | 365 a 370 MB | 466 a 479 MB |
| Primer contexto del proceso, hasta estar listo | 133 a 178 ms; 388 y 686 ms recién compilado | 383 a 392 ms; 3,5 s la primera vez |
| Primer uso hasta el primer cuadro | 156 a 170 ms; 664 ms la primera vez | — |
| Contextos siguientes, hasta estar listos | 24 a 26 ms | 356 a 373 ms |
| Contextos siguientes, hasta el primer cuadro | 44 a 61 ms | 417 a 503 ms |
| Clic del agente, a la vista y fuera de pantalla | 33–45 ms y 44–45 ms | 55 ms y 33–44 ms |
| Captura del agente, a la vista y fuera de pantalla | 66 ms y 66–155 ms | 88 ms y 55–66 ms |
| Cierre de un contexto | 2 a 3 ms | 15 a 20 ms |

Cómo se tomó cada una:

- **Cuadros por segundo.** Cuadros entregados a la vista durante 4 s en la
  página de prueba con una animación CSS, y con un paso de rueda cada 16 ms
  como los envía la ventana.
- **Rueda.** Desde que se envía el paso hasta el primer cuadro posterior; en
  reposo, 20 pasos separados por 450 ms de quietud.
- **Procesador.** Tiempo de usuario y de sistema de `/proc/<pid>/stat`
  durante esas ventanas, como porcentaje de un núcleo: el proceso de la
  prueba, que hace lo que haría la aplicación, y aparte los procesos del
  navegador. Con CEF la parte de navegador de Chromium corre dentro de la
  aplicación y cuenta ahí. El de la aplicación con CEF salió en dos niveles:
  37–42 % animando en seis corridas y 48 % en dos, con la máquina en uso; no
  se buscó la causa.
- **Memoria.** PSS de `/proc/<pid>/smaps_rollup`. Lo que suma la aplicación
  se cuenta desde antes de iniciar CEF, así que incluye a Chromium dentro del
  proceso; a la vista se agregan el lienzo (16 MiB), su copia RGB (12 MiB) y
  los cuadros en tránsito.
- **Primer uso hasta el primer cuadro.** Seis ejecuciones de `--cef-probe`
  con el binario `desktop`: desde `BrowserRuntime::start` hasta el primer
  cuadro, con CEF iniciándose. La primera, con `libcef.so` fuera de la caché
  de páginas, tardó 664 ms.
- **Clic y captura del agente.** Ocho llamadas a la herramienta por corrida.
  El cliente consulta cada 10 ms, así que los valores caen en múltiplos de
  11 ms.

Un cuadro entero de 2560 × 1600 le cuesta a la aplicación 2,8–3,0 ms en el
hilo de CEF (la copia) y 3,4 ms en el hilo de cuadros (conversión y armado);
con Chrome eran 8,0–8,1 ms de decodificar el JPEG. De ahí sale casi todo lo
que gasta la aplicación cuando cambia toda la vista: a 60 cuadros por segundo
son 6,3 ms por cuadro, 38 % de un núcleo. Cuando cambia una parte se copia y
se convierte sólo esa parte.

### Lo que cuesta un cambio según cuánto ocupa

Tomadas el 12 de octubre de 2026 con la prueba
`real_cef_measurements_of_what_a_change_costs_by_how_much_of_the_view_it_is`,
perfil `desktop`, sin ventana, viewport de 1280 × 800 a escala 2 (cuadros de
2560 × 1600). «Antes» es el rango de cinco corridas de la misma prueba
compilada sin el cambio, cuando cada pintura se copiaba, se convertía y se
entregaba entera; «ahora», el de ocho corridas (cinco, y tres más con el
código final). Cada corrida es un proceso nuevo, con ningún `rustc` en curso.
La carga de un minuto estaba entre 0,6 y 1,2 antes del cambio y entre 1,0 y
6,6 después: lo más alto, recién terminada una compilación, y esas corridas
dieron lo mismo que las demás. Son porcentajes de un núcleo.

| Qué cambia en la página | Chromium compone | Cuadros por segundo a la vista | Aplicación, antes | Aplicación, ahora | Procesos de Chromium, antes | Procesos de Chromium, ahora |
| --- | --- | --- | --- | --- | --- | --- |
| El cursor de texto que parpadea en un campo | 60 veces por segundo | 2,0 | 3,5 % | 1,5–2,0 % | 0,5–1,0 % | 0,5–1,2 % |
| Una esquina que gira (60 × 60 píxeles CSS) | 60 | 60,0–60,3 | 45,9–46,9 % | 4,0–4,5 % | 9,7–10,0 % | 9,7–10,5 % |
| Toda la vista (el fondo cambia de color) | 60 | 60,3 | 45,4–47,3 % | 42,1–44,1 % | 13,5–15,0 % | 13,5–14,2 % |
| Desplazamiento continuo de una página de texto | 60 | 59,2–59,7 | 48,9–50,7 % | 46,7–49,0 % | 51,2–52,5 % | 52,2–54,2 % |
| El cursor de texto | 120 | 2,0 | 2,2–2,7 % | 0,7–1,0 % | 0,5–1,0 % | 0,5–1,0 % |
| Una esquina que gira | 120 | 120,3–120,6 | 86,7–89,2 % | 7,0–8,0 % | 15,5–16,2 % | 18,7–19,9 % |
| Toda la vista | 120 | 120,3–120,4 | 88,2–89,7 % | 78,8–84,0 % | 23,7–24,7 % | 22,9–25,4 % |
| Desplazamiento continuo | 120 | 89,8–91,5 antes, 94,7–98,7 ahora | 75,1–76,0 % | 72,9–74,5 % | 84,0–85,0 % | 89,7–90,6 % |

Cómo se tomó:

- **La frecuencia.** La prueba fija la frecuencia a la que Chromium compone
  (`set_display_rate`, que sólo existe en las pruebas); en la aplicación la
  decide la ventana. Sin ventana son 60.
- **Las páginas.** `/form` con el foco en un campo, `/animated`, `/full` (una
  animación CSS del fondo, que cambia todos los píxeles en cada cuadro) y `/`
  con un paso de rueda por cuadro, de a uno en vuelo, como los envía la
  ventana. Cada ventana de medición dura 4 s, un segundo después de abrir la
  página.
- **El procesador.** Tiempo de usuario y de sistema de `/proc/<pid>/stat`:
  el proceso de la prueba y, aparte, los auxiliares de Chromium que cuelgan de
  él. La parte de navegador de Chromium corre dentro del proceso de la prueba
  y cuenta ahí.
- **Quién toma los cuadros.** Un cuadro se toma y se suelta apenas el runtime
  avisa que está listo, que es lo que hace la ventana en cada refresco. La
  subida a la GPU no está: no hay ventana. Ahí también se sube ahora sólo la
  parte que cambió (`present_surface` escribe un rectángulo de la textura),
  pero **cuánto baja eso no se midió**.
- **Las pruebas no comparan lienzos al medir.** La comparación de cada
  pintura con el búfer de Chromium, que hacen las demás pruebas, queda
  apagada en éstas.

Lo que muestran:

- Una animación chica cuesta la décima parte que antes: 4 % en lugar de 46 %
  a 60 cuadros por segundo, 8 % en lugar de 88 % a 120.
- Lo que cambia toda la vista no cuesta más que antes, y algo menos (unos 3
  puntos a 60 y entre 5 y 10 a 120). No es por enviar menos: en esta
  medición, copiar el búfer fila por fila gasta menos que copiarlo de una
  vez (probado en los dos sentidos, ver abajo). Se supone que es por cómo
  quedan los datos en la caché del procesador para la conversión que sigue;
  no se comprobó.
- El desplazamiento a 120 pasa de 90–92 a 95–99 cuadros por segundo: el
  límite sigue siendo Chromium, que compone por software.
- Con la esquina que gira a 120, los procesos de Chromium gastan unos 3
  puntos más que antes (19–20 % contra 16 %). No se buscó la causa.

Lo que se probó y se descartó: copiar de una vez las filas que cambian a lo
largo de más de la mitad de su ancho. En esta misma medición la vista entera
volvió a costar lo de antes (45,1–46,6 % a 60 y 86,0–90,5 % a 120) y el
desplazamiento también (50,0–51,5 % y 75,1–76,4 %, con 90–91 cuadros por
segundo a 120).

La prueba anterior, `real_cef_measurements_of_the_live_view…`, se repitió el
mismo día antes y después del cambio, tres corridas cada vez. En ella nadie
toma los cuadros. Los cuadros por segundo y la rueda no cambiaron: 60,0–60,1
y 59,8–60,0 animando, 59,6 desplazando las dos veces, de la rueda al
siguiente cuadro una mediana de 16,6–16,7 ms y de 16,7 ms (p90 17,0–17,1 las
dos veces), y de la rueda al primer cuadro en reposo 15,5–16,1 ms y
15,6–16,2 ms. El procesador animando bajó de 37–38 % a 5–6 %. Dos cosas
subieron en esa prueba:

- Desplazando, la aplicación gasta 42–43 % en lugar de 40 %. Con la copia de
  una vez volvía a 39–40 %: sin nadie que tome los cuadros, la copia fila por
  fila cuesta más en lugar de menos. Se eligió la que gasta menos cuando los
  cuadros se toman, que es lo que hace la ventana.
- Animando la esquina, el proceso suma 171–172 MB en lugar de 154–157 MB. No
  es memoria en uso: contada después de pedirle al asignador que devuelva lo
  que tiene libre (`malloc_trim`), la cifra es 140 MB. Ya no circulan cuadros
  enteros, y los últimos quedan un rato en el asignador sin volver al
  sistema. Con la página quieta la cifra es la de antes (153,7–155,0 MB
  contra 152,2–155,3 MB).

Tamaño:

| | |
| --- | --- |
| El ejecutable `desktop` | 197 KB más (211,7 MB contra 211,5 MB); sin símbolos, 265 KB más (85,7 MB contra 85,5 MB) |
| Archivos de CEF a distribuir | 358 MB: `libcef.so` 260 MB, `locales/` 48 MB, `resources.pak` 23 MB, `libvk_swiftshader.so` 15 MB, `icudtl.dat` 11 MB y el resto 4 MB |
| Un proceso que enlaza `libcef.so` sin usarlo | 58,2 a 59,4 MB de PSS contra 8,4 a 8,5 MB |

De esos 358 MB, sin GL por software `libvk_swiftshader.so` no se carga, y de
`locales/` haría falta sólo `en-US.pak`, el locale de CEF cuando no se le
indica otro (inferido): sin lo demás serían unos 296 MB. **No se probó
distribuir ese subconjunto.**

## Validación

Las pruebas rápidas no inician CEF, con la opción o sin ella. Con la opción
se agregan las de `browser_cef.rs`: el idioma que se anuncia a las páginas, la
frecuencia de la pantalla, cómo se dibuja una lista sobre la página y sobre
una parte de ella, también cuando asoma por un borde, que una lista que se
abre, cambia o se cierra llega a la vista aunque una captura del agente acabe
de repintar la página sin cambios, y cómo viaja lo pintado: como una parte
del cuadro, sin perder lo de un cuadro que la vista no tomó, entero después
de un cambio de tamaño o de ocultarse, retenido durante una captura. El aviso
del pie y su descarte se prueban sin la opción, en `browser_ui_tests.rs` y
`browser_host_tests.rs`.

Las pruebas contra el Chromium embebido están ignoradas por defecto. Son los
mismos escenarios que corren contra Chrome real en `browser_chrome_tests.rs`,
más los propios de CEF. No abren ventanas, no usan la GPU y sólo visitan
páginas que la prueba sirve en localhost:

```sh
nix-shell nix/shell-cef.nix --run \
  "cargo test -p agents-window --features cef -- --ignored real_cef --test-threads=1"
```

Un arnés de pruebas no tiene un `main` que atienda a los auxiliares de CEF:
las pruebas le indican a CEF el ejecutable de la aplicación que cargo compila
al lado (`browser_subprocess_path`), por eso el comando no lleva `--bin`.

- Agente y cliente externo: los pasos de la prueba de Chrome y, además,
  diálogos descartados y aceptados, `window.open` rechazado, el idioma
  anunciado y un marco de otro sitio leído y accionado por referencia.
- Vista en vivo a escala 2 y 1, entrada de la ventana, teclas modificadoras
  y teclado español: los mismos pasos. De la página que anima una esquina la
  vista recibe esa esquina y no cuadros enteros.
- Partes de cuadro, a escala 2, 1 y 1,5: después de escribir, de que el
  cursor parpadee (cada parpadeo es una parte de menos del 2 % del cuadro),
  de desplazar, de abrir una lista, pasar el puntero por sus filas y
  cerrarla, de una captura del agente entre dos cambios, de ocultar y volver
  a mostrar, de un cambio de tamaño y de una animación que se detiene, la
  textura de la vista es píxel por píxel la página como Chromium la pintó. Al
  final se la compara con el cuadro entero que recibe una vista nueva de la
  misma página, y son iguales.
- En todas estas pruebas, después de cada pintura parcial se compara el
  lienzo completo con el búfer de Chromium: el rectángulo que Chromium dice
  haber pintado fue siempre todo lo que cambió.
- Enlaces que piden otra pestaña: `target="_blank"`, `window.open` desde un
  clic, una ventana con nombre, un formulario hacia otra pestaña, Control +
  clic, Shift + clic y botón del medio se abren en la misma página y
  **Atrás** vuelve; el clic del agente recibe la página nueva; lo que la
  página intenta sola, y lo que no es una dirección web, no abre nada; cada
  dirección se pidió una sola vez.
- Lista de un `<select>`, a escala 2 y 1: está donde abre y mide lo que el
  control a esa escala, el cuadro que recibe la vista la muestra, las flechas,
  Enter y Escape actúan sobre ella, la fila bajo el puntero se resalta, un
  clic la elige, la rueda la recorre sin mover la página, apretar sobre el
  control y soltar sobre una fila la elige y la página sabe que el botón
  subió, y una vista ocultada con la lista abierta la recupera al volver.
- Cursor: mano sobre un enlace, barra de texto sobre un texto y un campo,
  prohibido, redimensionar columna y flecha sobre el fondo.
- Aislamiento: dos contextos iniciados a la vez en el mismo sitio no
  comparten cookies, `localStorage`, `sessionStorage`, IndexedDB ni caché
  HTTP; un contexto nuevo después de cerrar otro empieza vacío; ningún archivo
  del directorio de CEF contiene lo que guardó una página.
- Sin GPU: ningún proceso abre un dispositivo gráfico ni mapea un driver, y
  WebGL no existe.
- Señales: con CEF iniciado, SIGHUP, SIGINT y SIGTERM siguen sin manejador.
- `tests/browser_cef.rs` ejecuta el binario construido con `--cef-probe`: el
  propio ejecutable atiende a los auxiliares, se llega al primer cuadro y al
  salir no queda ningún proceso ni directorio. Y lo ejecuta con los recursos
  de CEF ausentes: no muere, lo dice, sigue con el navegador del sistema y el
  contexto lleva el motivo que muestra el pie del panel.

Las mediciones se repiten con el perfil optimizado, sin otra compilación en
curso:

```sh
nix-shell nix/shell-cef.nix --run \
  "cargo test --profile desktop -p agents-window --features cef --bin agents-window -- --ignored --nocapture --test-threads=1 real_cef_measurements"
```

Ese comando no recompila el ejecutable de la aplicación; tiene que existir de
antes, compilado con la opción y el mismo perfil. Son dos pruebas: la de la
vista en vivo y la de lo que cuesta un cambio según cuánto ocupa, que pide
120 cuadros por segundo además de 60 sin cambiar lo que usa la aplicación.

Validado el 12 de octubre de 2026 con CEF 154.0.34 (Chromium 154.0.8037.98) en
NixOS, sandbox activo: las dieciséis pruebas (catorce en el arnés, con las dos
de mediciones, y las dos de `tests/browser_cef.rs`) pasaron en tres corridas
seguidas y después de cada una no quedó ningún proceso de CEF ni directorio.

## Lo que queda para el paso de GPU

- **La fuente de píxeles.** `Embedded::paint` y `Embedded::frame` en
  `browser_cef.rs` son lo que se reemplaza: hoy copian un búfer y arman un
  `SurfaceFrame`; con `on_accelerated_paint` llega un DMA-BUF por cuadro. La
  entrega a la vista (`LiveView::publish`) y todo lo demás quedan. limn
  necesita una superficie que acepte una textura importada en lugar de
  píxeles.
- **La plataforma.** Los cuadros compartidos necesitan la plataforma de la
  sesión (Wayland o X11), no la headless. Con Wayland la escala de una vista
  sin ventana se ignora (lo midió la prueba de viabilidad) y la emulación no
  sirve; quedan el zoom de página al 200 %, que hay que verificar con el
  driver (las coordenadas de entrada y las capturas pueden cambiar), o la
  plataforma X11. Esas plataformas abren el nodo de render de la GPU ya en el
  proceso de la aplicación.
- **Los interruptores.** Quitar los tres que apagan la GPU en `openide-cef` y
  elegir en qué GPU corre Chromium: la prueba de viabilidad no obtuvo cuadros
  acelerados con Chromium en la NVIDIA.
- **La frecuencia.** Ya sigue a la ventana (ver Límites); el paso de GPU sólo
  tiene que conservarlo.
- **Las pruebas.** La que vigila que no se toque la GPU pasa a valer sólo para
  el camino por software, que conviene conservar como reemplazo.
