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

Abrir la pestaña no instala nada. **Instalar navegador** instala la biblioteca
y los binarios administrados; no añade bibliotecas al sistema ni modifica el
perfil del usuario. En NixOS hace falta un navegador empaquetado compatible, o
un entorno que proporcione las bibliotecas requeridas por el binario de
Playwright. [Instalación de navegadores](https://playwright.dev/docs/browsers).

## Interacción, errores y límites

La superficie usa una sola barra fija: atrás, adelante, recargar, dirección y
**Más opciones**. Enter abre la dirección; el menú reúne copiar dirección,
actualizar captura, copiar conexión MCP y reiniciar el contexto. Cada acción
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
El host envía ambos ejes del trackpad y convierte Shift + rueda vertical en
desplazamiento horizontal. Pegar y confirmar texto de un método de entrada
requieren que el viewport visible tenga foco; durante una navegación no se
envían al documento nuevo. PageUp y PageDown llegan a la página.
Las URLs `file:`, `javascript:` y similares no forman parte del protocolo.
Cada respuesta contiene la URL, título, historial, captura PNG y, cuando
corresponde, árbol accesible. Se rechazan localizadores ambiguos.

Para el agente, el árbol accesible lleva referencias de elemento (`[ref=e12]`)
y el bridge agrega puntero, selección, casillas, arrastre, esperas, evaluación
de JavaScript en la página, consola, red, capturas y respuesta a diálogos. El
catálogo, el ciclo de trabajo, los límites de cada dato y lo que se dejó fuera
están en [Herramientas del navegador](browser-tools.md). Un fallo que es la
respuesta de la propia herramienta (una referencia caducada, un script que
lanza un error, una espera agotada) viaja en `output.error` de una respuesta
correcta: no se muestra como error de página ni cuenta como efecto incierto.

La entrada de la ventana tiene una cola propia limitada a 128 comandos y
256 KiB, con hasta cuatro operaciones en vuelo. Agrupa texto Unicode y scroll
adyacentes sin cruzar clics, teclas o navegaciones. La cantidad pendiente se
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
autenticado. La vista usa capturas periódicas, no un WebView ni video continuo:
no promete la latencia, selección por arrastre o reproducción multimedia de un
navegador de escritorio completo.

Si el compositor de Chromium todavía no tiene un frame después de navegar,
se reintenta una vez **la captura**, nunca el clic o la escritura que la causó.
Una captura periódica exitosa no borra el error de navegación anterior. La
interfaz muestra una explicación breve con detalle al pasar el puntero; una
acción explícita posterior puede resolverla. La pérdida del proceso exige un
contexto nuevo, para conservar la identidad de las herramientas pendientes.
**Reintentar** conserva el destino HTTP/HTTPS y lo abre una sola vez cuando el
nuevo contexto está listo, incluso si se cambió de pestaña mientras arrancaba.
La barra conserva esa dirección durante `about:blank` y ante una página de
error interna; sólo una navegación confirmada puede sustituirla. No se
reproducen clics, formularios ni escritura de la instancia anterior.

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
reintento, limpieza de perfiles y transporte del proxy MCP. También comprueba
copia, corte en dos fases, rechazo si el campo cambia, campos de sólo lectura,
contraseñas, `contenteditable` y límites de texto. El broker del
último caso es una fixture local; no pretende probar el catálogo de permisos
Rust ni los eventos de la ventana nativa.

Validado el 5 de octubre de 2026 con Playwright 1.63.0 y Google Chrome
150.0.7871.46 empaquetado por NixOS, con sandbox habilitado: cuatro casos reales
pasaron. El 11 de octubre de 2026 pasaron los seis casos, con los dos de las
herramientas del agente. Las pruebas Rust de entorno, limpieza, límites y conservación del
error están en `browser_runtime_tests.rs`; las compila la suite central.
`browser_host_tests.rs` verifica además el foco, las entradas, el destino de
reintento entre conversaciones, la conservación de ráfagas de entrada y el
portapapeles entre conversaciones con un actor de prueba sin Node, navegador,
sockets ni acceso al entorno personal. `browser_input_app.rs` contiene las
regresiones de orden, agrupación y rechazo antes de modificar una cola llena.

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

La prueba `scripts/browser-desktop-e2e.py` cubre también el recorrido completo
desde la ventana Wayland real: arranque con `TMPDIR` eliminado, navegación al
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
