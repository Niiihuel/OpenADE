# Panel derecho y terminal inferior

El panel derecho comparte una barra de pestañas para archivos, repositorio,
cambios de la conversación, actividad y navegador. La terminal ocupa una
superficie inferior independiente: puede permanecer abierta mientras se usa un
archivo, el navegador o el repositorio a la derecha. Sus selecciones y controles
de ocultar no se sustituyen entre sí. Chat, editor y terminal comparten un marco
exterior con las cuatro esquinas redondeadas; los fondos de los hijos no lo
sobrescriben. Las divisorias internas permanecen rectas, y menús y tooltips se
dibujan por encima del marco. Las pestañas derechas forman una
franja continua con scroll horizontal; la rueda sobre la barra, Shift+rueda y
el gesto horizontal del touchpad recorren el contenido. La selección revela la
pestaña completa, incluido su cierre, al cambiar de pestaña o ancho. Una
reconstrucción con la misma selección conserva el desplazamiento manual. Los
botones de nueva pestaña, menú y ocultar panel permanecen fijos a la derecha.
El menú da acceso al conjunto completo y a la administración de abiertas y
cerradas recientemente.

La barra mide 32 px y las pestañas 28 px, con ancho según el título. La activa
comparte el fondo del contenido: tiene radio de 8 px sólo arriba y llega hasta
el cuerpo, sin margen inferior. Las inactivas descansan sobre una franja
neutra, con separadores discretos sólo entre ellas. El hover conserva los
tamaños y los cierres siempre reservan su espacio.
Los bordes del viewport se desvanecen en 16 px sólo cuando queda contenido
fuera de vista. El fade es pasivo y revelar una pestaña reserva espacio para
que su cierre siga visible. El cierre y las acciones fijas miden 24 px.
El fade vertical pertenece al contenido desplazable, nunca a toda la superficie:
aparece en sus primeros 12 px sólo cuando el desplazamiento vertical es mayor
que cero. Volver al comienzo lo retira. Cabeceras, breadcrumbs, botones,
gutter del editor y barras de desplazamiento quedan fuera de ese degradado;
no cambian sus hitboxes ni el foco.
El clic conserva foco de puntero; el teclado muestra un único contorno sobre
la pestaña completa. El botón de terminal alterna el panel inferior sin crear
sesiones; Ctrl + acento grave hace lo mismo.

- Clic en una pestaña para activarla; `×` o clic central para cerrarla.
- Arrastrar una pestaña sobre otra para cambiar el orden.
- Con foco en la barra, izquierda/derecha y Home/End seleccionan una pestaña.
- `Ctrl+Tab` y `Ctrl+Shift+Tab` recorren las pestañas derechas abiertas de la tarea,
  también las que quedan fuera del viewport horizontal.
- `+` abre una vista. «Nueva terminal» crea otra sesión PTY independiente abajo.
- Ocultar cualquiera de las superficies conserva sus pestañas y procesos. Cerrar una terminal libera
  su proceso; las terminales cerradas no se restauran como procesos nuevos.
- «Cerrar todas» y «Cerrar otras» del panel derecho conservan las terminales.

La promoción de un borrador a tarea conserva todas sus terminales y sus IDs. Las
pestañas compartidas pertenecen al proyecto; las de actividad, conversación y
terminal conservan el ámbito de su tarea.

El registro mantiene todas las identidades para administrar los recursos,
incluidos los de conversaciones ocultas. Las consultas `right_tabs` y
`terminal_tabs` proyectan cada superficie; `active_tab` y `active_terminal`
mantienen selecciones distintas. Abrir de nuevo la terminal conserva la última
seleccionada entre varias sesiones. Colapso y maximización de la terminal son
preferencias por conversación; su altura preferida es por proyecto. La altura
normal empieza en 240 px, usa un mínimo de 120 px cuando cabe y se limita al
65 % del espacio disponible, sin perder la preferencia al achicar la ventana.
La divisoria se dibuja encima de la cabecera inferior y coincide con el gesto
de resize. El divisor derecho termina allí; no atraviesa la terminal. Los menús
abiertos conservan prioridad sobre los divisores, y salir de la ventana cancela
el arrastre. Ampliar reserva espacio para el composer y, si está abierto, Entorno.
Esa reserva sigue el progreso de la animación de Entorno para evitar saltos del
divisor; con movimiento reducido cambia directamente al estado final.
Restaurar vuelve a la altura elegida, incluso tras reducir la ventana.

Los títulos largos se desvanecen en el borde disponible. En la lista lateral
de conversaciones se retiró la etiqueta de proveedor; el indicador de estado
ocupa una columna propia de 28 px, con el icono de 16 px separado del borde y
del título. El menú de acciones sigue disponible por puntero y teclado.

Los encabezados del editor y del detalle Git utilizan el componente nativo
`Breadcrumb`: separadores chevron de 14 px, centrados y pasivos, recurso actual diferenciado y títulos con
fade. En anchos estrechos prioriza la raíz y el recurso actual; las rutas
completas quedan en descripciones de tooltip que envuelven el texto. Sólo los
ancestros que reciben una acción real son interactivos. El editor usa una sola
barra: ruta, Buscar y Más. Guardar (Ctrl+S), Recargar, Símbolos y ajuste de líneas
están en el menú; la disponibilidad de guardar depende del documento y de sus
cambios. El indicador de cambios permanece en la pestaña y la ruta, y los errores
de persistencia conservan su aviso. No se reserva una franja adicional de comandos. Archivos presenta el
proyecto junto a Buscar y Actualizar; la terminal identifica la sesión activa
en su pestaña y muestra el directorio inicial en una ayuda, sin duplicar barras
de ruta.

El editor editable comparte con el IDE una sola barra de estado de 22 px,
separada del viewport por un borde de 1 px. Diagnósticos y servicios de lenguaje
quedan a la izquierda; cursor, selección, sangría, codificación, finales de línea
y lenguaje se distribuyen según el ancho, con las opciones restantes en Más.
Las celdas interactivas resaltan toda su altura. Posición abre Ir a línea;
lenguaje, sangría y finales de línea conservan sus acciones reales. El cursor se
actualiza también al navegar o seleccionar sin editar. Los menús se invalidan al
cambiar de documento o revisión y al ocultar el dock o abrir Entorno. El scroll
horizontal permanece dentro del editor, encima del pie. Las vistas previas sin
fuente visible no muestran controles de un documento editable.

LSP abre un popover compacto anclado a esa barra, compartido por Agentes e IDE.
Muestra el lenguaje y el estado real del servidor, permite indicar un ejecutable
opcional y conectar o reiniciar el servicio. Dejar la ruta vacía usa la detección
automática. Abrirlo conserva el área y el scroll del código; los ajustes de
sangría y finales de línea siguen en sus propias celdas. El campo recibe el foco,
Escape vuelve al botón LSP y los errores de conexión permanecen en el popover.
Cambiar de documento, revisión, proyecto o lenguaje invalida el formulario.

Problemas se abre en el panel inferior compartido con Terminal, Salida y
Consola, tanto en Agentes como en IDE. El editor conserva su cabecera y el
listado ya no ocupa una franja sobre el código. Cada fila muestra severidad,
mensaje y posición; navegar comprueba que el archivo y la revisión sigan
vigentes. Los estados pendiente, desactualizado y sin problemas son distintos.
Cambiar de sección conserva las sesiones de terminal; abrir Problemas no crea
procesos. Agentes mantiene las pestañas de sesiones cuando caben y el selector
de sesiones en anchos pequeños. El panel comparte altura, resize y animación;
el atajo de alternar el panel inferior recupera la sección seleccionada.

## Repositorio y diffs

El worker de revisión obtiene metadatos `git diff --raw --numstat -z`, sin
construir hunks de todos los archivos. El resumen se conserva durante una
actualización del mismo proyecto. La consulta de GitHub es independiente y sus
avisos aparecen debajo del listado local.

El listado de archivos es virtual y comparte el resumen inmutable del worker:
sólo construye las filas que entran en la ventana y un margen de scroll, sin
copiar todas las rutas al reconstruir el panel. Cada fila muestra primero el
nombre del archivo y debajo su carpeta. Seleccionar un archivo solicita su diff a un
worker. El componente `CodeDiff` conserva las líneas mediante `Arc`, limita el
trabajo de pintado al viewport, permite desplazamiento horizontal y mantiene el
texto completo de cada línea. Los archivos nuevos también se pueden inspeccionar.

Cada fila ofrece preparar o quitar de preparados. Las operaciones se ejecutan
en un worker separado y sólo a partir de un clic explícito. El host valida el
proyecto y la sección de la selección, evita envíos duplicados mientras hay una
operación pendiente y sólo refresca el proyecto al que pertenece la respuesta.
Las rutas usan pathspec literal; deshacer la preparación conserva el contenido
del archivo en disco, también en una rama sin primer commit. Abrir en editor
reutiliza la navegación normal de documentos.

Los cambios del chat mantienen su procedencia y cobertura del registro. Su
lista de archivos y el diff seleccionado usan scroll virtual independiente.

### Archivos del chat y capturas

Abrir un archivo desde una actividad resuelve nuevamente la fila original del
chat, su sesión y el índice del archivo. La acción no transporta una ruta que
pueda sustituir esos datos. Si el archivo existe dentro del proyecto, se abre
el documento editable; la comprobación incluye destinos de enlaces simbólicos.
Los documentos limpios se vuelven a comprobar al abrirlos explícitamente, sin
descartar cambios sin guardar.

Cuando el archivo falta y existe contenido registrado, se abre una pestaña
`CodeSnapshot` de solo lectura. Su aviso identifica una lectura capturada o una
propuesta y aclara que no representa el archivo en disco. Lecturas parciales,
parches y entradas todavía incompletas conservan la numeración de `CodeDiff`;
una entrada completa de `write_file` puede usar el `CodeEditor` propio, sin
controles de guardar ni aplicar. Abrir estas vistas no crea archivos.

Cada captura queda congelada al abrirla. Se reutiliza mientras su pestaña siga
abierta, conserva los límites del preview (32 KiB, 160 líneas y 8 archivos por
operación) y no toma contenido posterior del stream. Se retienen como máximo
16 capturas; cerrar una pestaña libera su vista y su fuente. Estas pestañas no
se restauran desde el historial de cierres porque su contenido no se persiste.

La separación entre documento, vista y captura sigue principios examinados en
[el buffer de Zed](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/text/src/text.rs#L59),
[su DisplayMap](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/src/display_map.rs#L1)
y [sus vistas que comparten buffer al dividirse](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/src/editor.rs#L1875).
También se estudiaron las
[transacciones y el viewport de CodeMirror 6](https://github.com/codemirror/website/blob/main/site/docs/guide/index.md)
y la distinción entre
[modelos, vistas y recursos liberables de Monaco](https://github.com/microsoft/monaco-editor#concepts).
Son referencias de arquitectura: el motor permanece en `openide-editor` y
`openide-ui`, sin incorporar esos editores ni GPUI.

## Terminal y actividad

La terminal comparte el componente de pestañas integradas del panel derecho:
cabecera de 32 px, pestañas de 28 px y radio superior de 8 px, con la sesión
activa unida al contenido. Las sesiones se eligen directamente en una franja con scroll
horizontal y fade de 16 px en los bordes que tienen contenido fuera de vista.
Cambiar de sesión o de ancho revela la pestaña activa junto con su cierre.
La rueda horizontal y Shift+rueda recorren la franja sin cambiar de sesión.

Cada `×` de pestaña termina sólo su PTY; el `×` fijo del extremo derecho oculta
todo el panel conservando los procesos. `+`, perfiles, acciones de sesión y
controles del panel permanecen fuera del scroll. Un botón compacto ofrece la
lista completa de sesiones como alternativa, sin ocupar la cabecera con un
título o selector grande. En ancho estrecho las acciones secundarias permanecen
disponibles en los menús. Cambiar de pestaña conserva su salida, PID y entorno.

`+` crea una sesión con el perfil configurado. El desplegable permite iniciar
otro perfil y abrir su configuración: sólo lista shells cuyo ejecutable se
resuelve en el sistema actual. El perfil predeterminado queda identificado.
Cambiar la preferencia no sustituye el shell de las sesiones ya creadas.
La búsqueda de perfiles ocurre al abrir ese menú, no en cada frame.

«Renombrar sesión» valida un nombre de 1 a 80 caracteres sin controles ni
saltos de línea. El nombre sigue el ID de la sesión mientras la aplicación
permanece abierta; no se atribuye al proceso ni se promete restauración tras
reiniciar la aplicación. Los menús y la edición del nombre conservan el ámbito
del proyecto y de la conversación que los abrió. Copiar, pegar, limpiar e
interrumpir operan sobre la sesión seleccionada, y el pegado usa el bridge
dirigido de portapapeles, sin simular Ctrl+V dentro del shell.

«Dividir terminal» presenta dos PTY independientes en columnas iguales. La nueva
sesión usa el mismo perfil congelado y el directorio inicial del proyecto;
no hereda las variables ni el directorio al que haya cambiado el shell vecino.
Cada viewport tiene identidad, foco, selección, scroll y tamaño de PTY propios.
Una franja superior pasiva de 2 px distingue el pane activo sin alterar su área
de entrada. El puntero cambia la sesión activa sin reordenar las columnas. La división
pertenece al ámbito de conversación/proyecto y acompaña la promoción del
borrador a tarea. Seleccionar otra sesión oculta temporalmente ese grupo.
«Quitar división» conserva ambos procesos y deja visible el activo;
cerrar un miembro conserva el otro. Esta implementación admite dos columnas
50/50, sin divisor interno ajustable ni divisiones anidadas.

La grilla se ajusta al área real y al ancho medido de la fuente. El margen
horizontal es de 12 px y el vertical de 4 px; pintura, dimensiones de la PTY
y selección con el ratón comparten esas medidas, también en terminales divididas.
Las líneas vacías emitidas por el shell forman parte de su salida y se conservan.
No hay un segundo editor de comandos debajo de la PTY.
El cursor de cada terminal indica su posición de entrada sin dibujar un borde
alrededor de toda la grilla. La pestaña seleccionada identifica la sesión activa. Los botones
de la barra conservan sus indicadores de foco y tooltips.

La PTY declara `TERM=xterm-256color`, respeta application-cursor y bracketed
paste, permite seleccionar texto y copiar con `Ctrl+Shift+C`. `Ctrl+C` copia una
selección o interrumpe el proceso si no hay selección. Escribir vuelve al final
del historial; el botón de historial permite hacerlo sin enviar entrada. La
rueda en pantalla alterna navega el TUI. El lector solicita repintado al recibir
salida, y los procesos de pestañas ocultas no repintan la ventana. La cola de
entrada agrupa bytes en orden con un límite pendiente de 1 MiB: una ráfaga de
teclas no agota una cantidad fija de eventos. El host toma un solo snapshot
tras procesar el lote de entrada; las escrituras a la PTY ocurren fuera del
lock de la cola y del hilo visual.

La traducción de entrada descarta el fallback físico de una tecla de control
cuando el mismo keypress trae texto imprimible. Esto evita que teclados
virtuales o remapeados envíen Retroceso o Enter junto con signos. Las teclas
reales, sus releases y los atajos Ctrl/Command conservan su comportamiento.

Actividad muestra una lista virtual de herramientas, el detalle seleccionado y
accesos a procesos, historial y permisos de sesión. Los argumentos y las salidas
tienen límites explícitos de previsualización y se dibujan por filas visibles.
Los límites no modifican el registro persistente.

## Navegador

El navegador usa una única barra fija con historial, recarga, dirección y menú
de acciones secundarias. No repite el título del sitio bajo la dirección ni
añade una segunda barra de scroll alrededor de la página. Un estado de error
propio reemplaza completamente una navegación fallida, incluida la captura
blanca de Chrome. Para localhost se explica que hay que verificar el servidor
y puerto; no se inicia un servidor automáticamente. Los detalles de aislamiento,
entrada, portapapeles, límites y validación están en
[Navegador del proyecto](browser-runtime.md).

## Referencias de diseño

Implementación propia, estudiando estructura e interacciones de:

- Zed: `references/zed/crates/git_ui/src/git_panel.rs`, paneles de terminal y
  `references/zed/crates/terminal/src/terminal.rs`: listas virtuales, trabajo en
  segundo plano, sesiones y modos ANSI.
- ZCode: `SidePaneTabTrigger.tsx`, `SidePaneTerminalPane.tsx`, `GitPane.tsx`,
  `GitPaneChangeCard.tsx` y `DiffsWorkerPoolProvider.tsx` dentro de
  `references/zcode`: pestañas compactas, cierre por pestaña y diffs diferidos.
- Code OSS: [terminalView.ts](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/contrib/terminal/browser/terminalView.ts)
  construye el selector de sesiones y el control combinado de nueva terminal;
  [terminalMenus.ts](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/contrib/terminal/browser/terminalMenus.ts)
  separa perfiles, acciones de sesión y configuración. La copia local consultada
  está en `/home/nihuel/projects/personal/refs/vscode`. Son referencias de
  interacción, no una incorporación de xterm.js o del código de VS Code.
- [Rust/UI Breadcrumb](https://rust-ui.com/docs/components/breadcrumb):
  jerarquía de enlaces, recurso actual pasivo y separadores chevron. El
  componente de OpenIDE es una implementación propia para `openide-ui`.
- [Warp Code Review](https://docs.warp.dev/code/code-review): revisión desde el
  panel derecho, navegación entre archivos y un diff por selección.
- [Warp tabs](https://docs.warp.dev/terminal/windows/tabs) y
  [split panes](https://docs.warp.dev/terminal/windows/split-panes): sesiones
  independientes y navegación entre superficies.

Pruebas de regresión cubren 10.000 archivos, selección y pegado en PTY real,
promoción de múltiples terminales, alcance de respuestas asíncronas y geometría
de pestañas entre 320 y 900 px. Para inspeccionar capturas con las fuentes de la
aplicación: `cargo test -p agents-window export_dock_visual_review -- --ignored`.
Los exports `export_session_rows_visual_review` y
`export_breadcrumb_and_tab_strip_visual_review` inspeccionan filas de sesiones,
fade, breadcrumbs y barra horizontal con fuentes reales. Las capturas se guardan
en `/tmp/openade-dock-review/`.

`dock_integration_tests.rs` comprueba las acciones del host: búsqueda de
archivos, menú y dirección del navegador, selección y colapso independientes de
terminal, cambio de conversación y cierre colectivo de las pestañas derechas.
Su fixture no lanza un shell ni usa perfiles personales del navegador.
`terminal_controls_app_tests.rs` cubre identidades, perfiles y edición de nombres;
`terminal_dock_ui_tests.rs` verifica geometría, menús, acciones dirigidas y
foco. `openide-ui/src/top_fade_tests.rs` comprueba que el degradado aparece sólo
después del scroll y que no cubre cabeceras, controles ni barras.


La prueba nativa `scripts/dock-desktop-e2e.py` usa la ventana real, teclado y
puntero, un servidor HTTP local y un proyecto temporal. Comprueba edición,
guardado desde el menú contextual y con Ctrl+S, PIDs y entorno de dos shells,
ocultar/reabrir, redimensionar, ampliar/restaurar sin recortar el composer, recuperación
del navegador y apertura desde Archivos. El observador sólo se compila con
`editor-e2e`, exige un directorio marcado y no recibe acciones por archivos.
Las pruebas se detienen si la ventana pierde foco.

```sh
rtk cargo build --profile desktop -p agents-window --features editor-e2e
rtk cargo test -p agents-window export_bottom_terminal_visual_review -- --ignored
```

El export de terminales escribe estados en 320, 620 y 1100 px a
`/tmp/openade-bottom-terminal-review`. Estas comprobaciones validan interacción
y geometría; no certifican una frecuencia sostenida de 120 fps.
La iteración de pestañas compartidas pasó 2560 pruebas automatizadas, con 94
pruebas ignoradas en la ejecución general. Se generaron 27 PNG: terminal activa,
historial, proceso terminado, overflow, acciones, perfiles, renombrado, lista de
sesiones y split en los tres anchos citados.


Para ejecutar la prueba sin ocupar el foco del escritorio,
`scripts/headless-desktop-fixture.py` crea una sesión privada KWin virtual →
Hyprland. Se verificó con KWin 6.7.2; requiere ambos compositores, sus dependencias
gráficas, `wtype` y `grim`. Un cliente Wayland de puntero virtual persistente
permite clics y arrastres sin uinput; teclado, portapapeles y captura usan sólo
el socket privado. El runner exige un runtime marcado de acceso privado,
limpia únicamente sus grupos de procesos y conserva diagnósticos de arranque.

```sh
rtk python3 scripts/headless-desktop-fixture.py \
  --kwin /ruta/a/kwin_wayland --output /tmp/openide-headless-dock \
  -- python3 scripts/dock-desktop-e2e.py --binary target/desktop/agents-window \
  --output /tmp/openide-headless-dock
```

El mismo runner admite `browser-desktop-e2e.py` para comprobar entrada Unicode
y copia, corte y pegado con el portapapeles de esa sesión aislada.

La ampliación de `dock-desktop-e2e.py` para la cabecera nueva selecciona las
sesiones directamente en sus pestañas y las termina desde su propio `×`.
A 520 px, mínimo permitido por la ventana nativa, comprueba overflow de tres
sesiones, rueda horizontal y que la pestaña activa
conserve el cierre completo, y después restaura el ancho. Añade un
flujo de división con dos PTY: comprueba PIDs diferentes, entrada y salida
aisladas, geometría sin solapes y conservación de ambas sesiones al volver a
una sola vista. También renombra una sesión conservando su PID y entorno,
comprueba el foco inicial del formulario sin hacer clic en el campo, abre y
cierra perfiles con Escape y crea una sesión independiente desde el perfil
predeterminado.

El 6 de octubre de 2026, la corrida de la cabecera final pasó los 13 flujos
nativos. El reporte quedó en `/tmp/openide-terminal-tabs-native/report.json`.
Incluye renombrado con foco automático, perfiles y Escape, selección directa,
overflow a 520 px, cierre de una pestaña inactiva y conservación del PID y
entorno de su vecina, además del split y las regresiones de navegador, archivos
y cuenta. `native-scroll-events.json` registra el gesto Wayland como rueda
horizontal real y su traducción a `Scroll2D`.

Se inspeccionaron `terminal-tabs-overflow-start-native.png`,
`terminal-tabs-overflow-end-native.png`, `terminal-rename-native.png`,
`terminal-profiles-native.png` y `terminal-split-native.png` del mismo
directorio. Las capturas confirman los extremos de la franja, los cierres y
la marca del pane activo sin solapes. La corrida anterior de Contexto conserva
su evidencia independiente en `/tmp/openide-context-native/report.json`:
tres flujos, foco tras Escape, consumo reportado por HTTP y scroll en
520 × 420 px. Las corridas usan exclusivamente el compositor y el portapapeles
privados, sin cuentas ni perfiles personales y sin reiniciar la aplicación
del usuario ni sus PTY.
