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
cuadro sin comprimir, a la escala de la ventana. La segunda mitad, cuadros
compartidos por GPU a la frecuencia de la pantalla, no está hecha.

**Nadie vio todavía un cuadro de esto en la ventana real.** Todo lo que sigue
se comprobó sin ventana: con pruebas, con mediciones a través del runtime y
mirando volcados PNG de los cuadros que recibiría la vista.

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
| `apps/agents-window/src/browser_cef.rs` | El contexto embebido del runtime: abre la página, guarda su último cuadro y lo convierte en el que toma la vista. |

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
de toda la vista. Ahí sólo se copia a un lienzo (2,9 ms a 2560 × 1600) y se
despierta al hilo de cuadros del runtime, que lo convierte a RGB y arma el
`SurfaceFrame` (3,4 ms). La entrega a la vista es la de siempre,
`LiveView::publish`, en un solo lugar. No hay JPEG ni PNG ni `screencast`.
Hay un solo lienzo: un cuadro que la vista no tomó queda tapado por el
siguiente, y nada se encola. Las respuestas al agente comparten hilo con la
copia y esperan a lo sumo una.

- **Oculta.** La página no se oculta para CEF: una página oculta deja de
  animar y de avanzar para el agente. Se descartan sus cuadros, se suelta el
  lienzo y Chromium compone a 10 cuadros por segundo en lugar de 60. Mientras
  corre un comando vuelve a 60, porque un clic o una captura esperan uno o
  dos cuadros.
- **Al mostrarse** se pide un pintado completo: la vista recibe un cuadro
  aunque la página esté quieta.
- **Capturas del agente.** Chromium vuelve a pintar la página en cada captura
  y, si es de un elemento o de la página completa, la dibuja distinta por un
  momento. Los cuadros de ese momento se retienen igual que con el flujo
  (durante la captura y 400 ms después) y lo que queda sólo se muestra si
  difiere de lo que la vista ya tenía.
- **Cambio de tamaño.** Primero cambia la vista y después la página; puede
  verse un cuadro intermedio.

**Entrada.** Sigue pasando por los eventos del protocolo del driver. Medido
contra la llamada nativa de CEF para la rueda: con la página quieta, del
evento al primer cuadro pasan 15,4–16,6 ms de mediana por el driver y
16,1–16,8 ms por CEF. Desplazando, la llamada nativa da 8,2–8,6 ms hasta el
cuadro siguiente contra 16,6–16,7 ms, pero mide otra cosa: Chromium anima el
desplazamiento de la rueda nativa, así que siempre hay un cuadro en camino,
muestre o no ese evento. No hay una ventaja medida que justifique dos caminos
de entrada.

**Salida.** Al terminar el proceso por cualquier salida normal se cierran las
páginas que queden, se cierra CEF y se borra su directorio. Si el proceso
muere sin eso (probado con SIGTERM, SIGHUP y SIGKILL), los auxiliares terminan
solos al perder a su padre y el directorio lo recoge el siguiente arranque.
Las señales terminan la aplicación igual que sin CEF.

## Si CEF no arranca

El contexto se inicia con el Chrome del sistema, igual que sin la opción. La
ventana no avisa nada; el motivo queda una vez en la salida de error:
«OpenIDE: el navegador embebido (CEF) no pudo iniciarse y se usa el navegador
del sistema. …». Si tampoco hay un Chrome instalado se ve el panel de siempre,
«No se encontró un navegador».

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
- 60 cuadros por segundo, no los 120 de la pantalla. A 120 la animación de
  prueba llegó a 120,1 con 65 % de un núcleo en la aplicación en lugar de
  37 %, y 18 % en lugar de 10 % en los procesos de Chromium (una corrida); es
  la constante `FRAME_RATE` de `browser_cef.rs`.
- Cada cuadro se copia y se convierte entero aunque cambie un píxel: una
  animación chica cuesta lo mismo que una de pantalla completa.
- La lista de un `<select>` abierta desde la ventana no se dibuja (CEF la
  pinta aparte). `browser_select_option` funciona.
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
Cada cifra es el rango de cuatro corridas con CEF y de tres con Chrome
150.0.7871.46, cada una en un proceso nuevo. Los tiempos hasta un cuadro son
hasta el cuadro listo para la ventana.

| Medida | CEF, por software | Chrome headless |
| --- | --- | --- |
| Cuadros por segundo, página animando | 60,0 a 60,1 | 35,6 a 35,9 |
| Cuadros por segundo, desplazamiento continuo | 59,7 | 30,5 a 31,5 |
| De la rueda al siguiente cuadro, desplazando | mediana 16,6–16,7 ms, p90 17,0–17,1 | mediana 31,0–31,7 ms, p90 48–62 |
| De la rueda al primer cuadro, página quieta | mediana 15,4–16,6 ms, p90 16,1–17,7 | mediana 63,0–63,7 ms, p90 64,6–65,6 |
| Procesador mientras anima: la aplicación | 37 a 42 % de un núcleo | 37 a 38 % |
| Procesador mientras anima: procesos del navegador | 10 % | 240 a 241 % |
| Procesador desplazando: aplicación y navegador | 40–46 % y 51–52 % | 36–37 % y 234–238 % |
| Procesador animando fuera de pantalla | 1 % y 1–2 % | 1 % y 8 % |
| Memoria que suma la aplicación, vista oculta | 83,0 a 86,3 MB | 2,8 a 3,2 MB |
| Memoria que suma la aplicación, página quieta a la vista | 149,7 a 153,1 MB | 61,3 a 61,5 MB |
| Memoria de los procesos del navegador, vista oculta | 220 a 222 MB en 9 | 318 a 327 MB en 11 |
| Memoria de los procesos del navegador, animando | 214 a 215 MB | 400 a 413 MB |
| Total con la vista oculta | 303 a 308 MB | 321 a 330 MB |
| Total animando a la vista | 366 a 370 MB | 466 a 479 MB |
| Primer contexto del proceso, hasta estar listo | 133 a 178 ms; 686 ms la primera vez | 383 a 392 ms; 3,5 s la primera vez |
| Primer uso hasta el primer cuadro | 156 a 170 ms; 664 ms la primera vez | — |
| Contextos siguientes, hasta estar listos | 24 a 26 ms | 356 a 373 ms |
| Contextos siguientes, hasta el primer cuadro | 44 a 47 ms | 417 a 503 ms |
| Clic del agente, a la vista y fuera de pantalla | 33–45 ms y 44 ms | 55 ms y 33–44 ms |
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
  aplicación y cuenta ahí.
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

Un cuadro de 2560 × 1600 le cuesta a la aplicación 2,8–3,0 ms en el hilo de
CEF (la copia) y 3,4 ms en el hilo de cuadros (conversión y armado); con
Chrome eran 8,0–8,1 ms de decodificar el JPEG. De ahí sale casi todo el 37 %
de un núcleo: a 60 cuadros por segundo son 6,3 ms por cuadro.

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

Las pruebas rápidas no cambian y no inician CEF, con la opción o sin ella.
Con la opción se agrega una, la del idioma que se anuncia a las páginas.

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
- Vista en vivo a escala 2 y 1, entrada de la ventana: los mismos pasos.
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
  de CEF ausentes: no muere, lo dice y sigue con el navegador del sistema.

Las mediciones se repiten con el perfil optimizado, sin otra compilación en
curso:

```sh
nix-shell nix/shell-cef.nix --run \
  "cargo test --profile desktop -p agents-window --features cef --bin agents-window -- --ignored --nocapture real_cef_measurements"
```

Ese comando no recompila el ejecutable de la aplicación; tiene que existir de
antes, compilado con la opción y el mismo perfil.

Validado el 11 de octubre de 2026 con CEF 154.0.34 (Chromium 154.0.8037.98) en
NixOS, sandbox activo: las nueve pruebas pasaron en tres corridas seguidas y
después de cada una no quedó ningún proceso de CEF ni directorio.

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
- **La frecuencia.** La ventana tiene que decirle al runtime la frecuencia de
  su pantalla; hoy es una constante.
- **Las pruebas.** La que vigila que no se toque la GPU pasa a valer sólo para
  el camino por software, que conviene conservar como reemplazo.
