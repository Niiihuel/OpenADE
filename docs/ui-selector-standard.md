# Selectores y popovers

Los selectores de OpenIDE usan medidas compartidas de `openide-ui` mediante
`SelectorSizing`, definido en `crates/openide-ui/src/selector_sizing.rs`.
Las medidas son píxeles lógicos: el renderer aplica el factor de escala de
la ventana. Una captura puede mostrar más píxeles físicos.

## Perfiles

- **COMPACT**: ancho preferido de 256 px, mínimo de 200 px. Se usa para
  modo, permisos y el menú inicial de modelo/esfuerzo. Hasta ocho entradas
  antes de incorporar scroll.
- **CATALOG**: ancho preferido de 320 px, mínimo de 220 px. Se usa para
  proveedores, modelos, proyectos y ramas con búsqueda. Hasta seis entradas
  antes de incorporar scroll.

`fit_width(available)` acota el ancho del contenido al espacio que ofrece
el host. `Popover` ajusta la superficie final al viewport y puede cambiar de
lado. No se fija un alto grande para una lista corta.

## Filas y controles

- Fila con una etiqueta: **28 px**.
- Fila con un identificador breve en segunda línea: **40 px**.
- Cabecera y buscador: **28 px** cada uno.
- Iconos: **16 px**; etiqueta principal: **12 px**; detalle: **11 px**.
- Espaciado entre icono y texto: **6 px**; inset horizontal de fila: **6 px**.
- En permisos, separación vertical entre superficies de opciones: **4 px**.
  El divisor mide **1 px**, con **4 px** libres por encima y por debajo.
  Ese espacio no pertenece al área clicable ni al hover de ninguna opción.
- El popover compartido aporta **6 px** de inset por lado.
- Carril para scroll: **14 px**, separado del texto y del check. En permisos
  sólo se reserva cuando las opciones desbordan la altura disponible; las
  listas que entran completas aprovechan ese ancho para las filas.

El contenido crece hasta su altura natural; las listas largas tienen scroll.
La cabecera/búsqueda y las acciones de pie permanecen fuera de la lista.
En ventanas bajas, el viewport de opciones se reduce y conserva navegación
por teclado, rueda y barra. Las filas propias de un catálogo deben declarar
su altura también en el wrapper del tooltip para medir su presupuesto.

El trigger conserva su tamaño dentro de la barra. En el composer, el selector
de modo y permisos tiene 28 px de alto y se centra en el carril de 28 px
del botón `+`. Su etiqueta se reduce a un icono cuando falta espacio. El editor
usa todo el ancho de la tarjeta, por encima de esa fila de controles; modelo,
esfuerzo, micrófono y envío se alinean a la derecha de la fila inferior.
Modelo, esfuerzo y permisos se presentan como texto sin una cápsula de fondo
permanente. Comparten paleta, radio y transición del menú de modelo/esfuerzo.
En el inicio de escritorio, el composer vacío ocupa una sola fila y los
permisos aparecen junto al contexto debajo. Al escribir o adjuntar archivos,
el texto pasa arriba y los permisos vuelven a la barra inferior. El botón de
ampliar conserva el borrador y el cursor; borrar todo recupera la barra vacía.

La tarjeta del composer usa radio de 10 px, texto de entrada de 13 px y
acciones secundarias de 24 px. Las sugerencias de archivos y comandos usan
filas de 42 px con nombre y detalle; su pie reutiliza las mismas teclas visuales
que los menús. El mensaje propio conserva sus párrafos con texto de 14 px,
interlínea de 21 px, inset de 10 px y radio de 8 px, sin repetir «Tú» ni el número
de turno sobre cada mensaje.

## Hover y acciones en toda la interfaz

`ComponentPalette::interactive_surface(element)` es el tratamiento compartido
para acciones propias: radio de **4 px**, transición de **120 ms**, feedback al
presionar y geometría estable. Aplica el estilo al botón real dentro de `Tooltip`
o del ancla de `Popover`; no habilita paneles desactivados ni cambia foco o IDs.
`Button` y los componentes de actividad usan esos valores. Los menús de
acciones tienen una densidad y un foco propios, descritos abajo.

En el host, `theme::interactive_surface` usa **INPUT** al pasar el puntero y
**SELECTED** al presionar, como el selector de permisos. Sobre tarjetas, sidebar
y superficies elevadas se usa la paleta contextual con el mismo tratamiento:
el hover debe seguir contrastando con el fondo. La selección de filas y las
acciones primarias o destructivas conservan sus colores; los tabs mantienen su
indicador y los botones circulares su forma.

Los botones con texto, los botones de icono, los campos, los buscadores y las
filas de opciones comparten esquinas discretas de **4 px** en reposo, hover,
pulsación y foco. Los segmentos usan ese radio dentro de una pista de **6 px**.
Los popovers, menús contextuales y tooltips comparten **6 px** en su superficie
exterior, sin cambiar sus insets ni zonas de interacción. El radio no se deriva
del padding ni aumenta con la altura del control. Los avatares, indicadores
circulares y botones de envío mantienen sus radios explícitos; el switch
conserva la geometría redonda de su pista y deslizador.

Las acciones compactas normales tienen un área de **32 × 32 px**, icono de
**16 px** y margen de **8 px**. Si una barra necesita una densidad menor, el
margen se calcula desde su área real para conservar el icono centrado. El hover
no debe cubrir el control vecino ni recortar iconos, texto o menús. Los hovers
no reconstruyen las vistas; las transiciones se detienen y respetan movimiento
reducido mediante el renderer compartido.

## Menús contextuales y edición de texto

Los menús de acciones comparten la convención de VS Code mediante
`DropdownMenu::context_menu_style()` y `menu::action_style`: ancho de **352 px**
limitado por el viewport, filas de al menos **26 px**,
inset de **4 px**, texto de **13 px**, keycaps compartidas de **20 px** de alto y
hover con radio de **4 px**. La superficie conserva radio de **6 px**, borde
fino y sombra. Los iconos, checks y flechas de submenú tienen columnas
reservadas; las etiquetas y los aceleradores no saltan cuando una opción está
marcada. Los encabezados de grupos son discretos. Cada grupo posterior al
primero incorpora un divisor; si el host ya declaró `Separator`, no se duplica.
Los separadores vacíos al inicio y al final se omiten. La lista crece hasta el
límite de **384 px**. El renderer acota la superficie al espacio disponible
por encima o debajo del disparador, sin cubrirlo. Incluso una lista menor que
ese límite conserva un viewport de scroll: al reducir la ventana, aparece el
carril y todas las acciones siguen siendo accesibles por rueda y teclado.

Un check representa el valor activado, sin pintar un segundo fondo de hover.
Sólo la fila recorrida por puntero o teclado tiene el fondo activo. El marcador
compartido `Element::action_menu_item()` permite al motor transferir ese estado
al alternar entre ambas entradas, sin conservar un foco pintado en otra fila.
La transición entre filas es inmediata: no quedan dos resaltados mientras el
puntero cruza el menú. Las acciones deshabilitadas no entran en el recorrido.

`MenuItem`, los menús contextuales y los menús anclados usan estas mismas
medidas. El anclaje, los IDs y las acciones siguen perteneciendo al host.
Los selectores de catálogo continúan utilizando sus perfiles `SelectorSizing`;
su selección persistente comunica el valor elegido y no cambia a una lista de
comandos. Un popover informativo también conserva su estructura propia.

La referencia revisada es
[`menu.ts` de VS Code](https://github.com/microsoft/vscode/blob/main/src/vs/base/browser/ui/menu/menu.ts),
en `/home/nihuel/projects/personal/refs/vscode/`: `getMenuWidgetCSS` define la
superficie compacta y `applyStyle` separa la marca de activación del foco. Sus
filas web son de 24 px; OpenIDE usa 26 px para conservar la lectura con sus
métricas nativas. Las pruebas `checked_actions_use_checkmarks_and_only_the_active_row_is_highlighted`,
`action_menu_keycaps_share_a_trailing_column_with_checked_rows` y
`pointer_and_keyboard_share_one_context_menu_active_row` cubren esa separación,
alineación y transferencia del estado activo. Las regresiones de ventanas
bajas también comprueban que el menú no tape su disparador y que End, Enter y
la rueda alcancen la última opción; las de submenús verifican Left/Escape
seguidos antes de un nuevo frame y cierres de listas sólo deshabilitadas.

`ContextMenu` presenta acciones sobre el contenido señalado por clic derecho;
**Shift+F10** lo abre desde el control enfocado. Conserva la selección y el
cursor existentes. Las flechas recorren opciones habilitadas, Enter ejecuta y
Escape cierra. El menú tiene **352 px lógicos** de ancho y un máximo de **384 px** de alto
o el 75 % del viewport, lo que sea menor. Incorpora scroll y revela la opción
seleccionada por teclado. Los atajos se muestran a la derecha con el mismo
`Kbd` que usan tooltips y buscadores: cada tecla tiene borde, radio de **4 px**,
alto de **20 px** y texto de **11 px**. Los signos `+` separan modificadores;
un espacio distingue acordes consecutivos. Esta elección unifica los menús y
las ayudas de OpenIDE, aunque los menús de VS Code usen texto plano.
El clic derecho del editor muestra
las acciones frecuentes; «Más acciones» conserva el catálogo agrupado.
El nombre completo de la acción y el acorde completo permanecen visibles en el
ancho normal. Las etiquetas largas ocupan varias líneas junto a las keycaps;
la altura se calcula con las fuentes nativas y el scroll usa esa altura real.
Sólo un viewport muy estrecho puede omitir el acorde completo para conservar
una columna legible de texto; nunca se muestran teclas parciales. Si una opción
supera por sí sola el alto del menú, la navegación
revela su comienzo y la rueda permite leer el resto.

Las pruebas `compact_menu_shortcuts_share_tooltip_keycaps_without_partial_chords`,
`compact_menu_shortcuts_are_visible_and_centered_inside_the_action` y
`grouped_menus_paint_one_divider_between_groups_without_empty_edges` verifican
el pintor compartido, el centrado y las separaciones entre grupos. El ancho
real de cada tecla se mide con la misma fuente nativa del tooltip.

Los campos de texto y `CodeEditor` ofrecen **Copiar**, **Cortar**, **Pegar** y
**Seleccionar todo**, con acciones no aplicables deshabilitadas. El editor de
sólo lectura y el texto estático permiten copiar y seleccionar, sin modificar
contenido. Los campos de contraseña no permiten copiar ni cortar desde el menú.
Abrirlo no lee el portapapeles: el host procesa `CopyRequested` y `PasteRequested`,
y devuelve `PasteTo` al destino que conserva foco y ámbito.

Para acciones propias se usa
`ContextMenu::new(id, child, entries, palette).render()`. Las entradas son
`ContextMenuEntry::Item(ContextMenuItem::new(action_id, label))` o `Separator`;
los items admiten `.shortcut(...)`, `.icon(...)` y `.enabled(...)`. La terminal
lo usa para copiar selección/salida, pegar y limpiar, con IDs de pestaña
validados por el host. Pegar conserva el protocolo de la PTY; **Ctrl+C** sigue
siendo interrupción cuando no hay selección que copiar.

`UiTree` conserva la apertura; `set_context_menu_palette(...)` configura los
menús automáticos y `context_menu_open()`/`dismiss_context_menu()` permiten
coordinar otros controles. La apertura y el cierre del menú contextual son instantáneos; el resaltado
entre sus filas también. Los popovers anclados conservan su animación de
apertura y respetan `set_reduced_motion(true)`. El cursor de texto queda
estable con movimiento reducido y deja de parpadear al perder foco de ventana.

La referencia de interacción es [Context Menu de Rust/UI](https://rust-ui.com/docs/components/context-menu).
Una [Menubar](https://rust-ui.com/docs/components/menubar) organiza comandos en
una barra visible; un [Popover](https://rust-ui.com/docs/components/popover)
abre contenido desde un botón. El menú contextual se ancla al contenido sobre
el que se actúa. Estos enlaces son referencias de UX; la API anterior es la
implementación nativa de `openide-ui`.

## Información visible y ayuda

El menú muestra nombre, icono, selección y, en un catálogo, el ID breve.
Las explicaciones de permisos, capacidades, autenticación, límites y precios
se presentan con `Tooltip`. No se agregan párrafos de instrucciones a las
filas ni una introducción extensa sobre el listado.

La ayuda se abre al costado del menú, con un ancho máximo de **320 px**,
al pasar el puntero o enfocar mediante teclado. El renderer usa el borde del
popover como referencia para evitar cubrir otras opciones. Los controles
deshabilitados conservan ayuda de hover y no agregan una acción de clic.
Un metadato desconocido se presenta como **Dato no disponible**.

El selector de modelo y esfuerzo se actualiza cuando llega el listado de cuenta
o cambia el catálogo, también si el chat ya está abierto. Un esfuerzo guardado
se recupera sólo si el proveedor anuncia ese nivel para el ID exacto elegido.
Mientras se comprueba la conexión se conserva la última configuración conocida;
la comprobación sigue controlando la disponibilidad de las acciones. Durante
una respuesta se mantienen modelo, esfuerzo y opciones del composer; los nuevos
metadatos se aplican cuando termina. Cargar metadatos no guarda preferencias,
cambia la ruta del motor ni envía un mensaje.

Plan tiene su propia selección; un divisor lo separa de los presets de
permisos. La selección visual refleja los estados reales del host.

El preset **Permitir todo** reemplaza la entrada Personalizado del composer.
Seleccionarlo abre `AlertDialog`, acotado a 440 px y al viewport. Confirmar
habilita edición y comandos para los próximos turnos; cancelar, Escape o
una confirmación de otro chat/carpeta no cambian los permisos. Sólo lectura
y Pedir aprobación revocan el acceso completo. La administración en Linux
usa autenticación por comando y requiere un `pkexec` habilitado por el sistema;
la aplicación de escritorio conserva los permisos de su usuario. Las reglas
de comandos guardadas y las aprobaciones externas siguen aplicándose.

## API

```rust
let sizing = SelectorSizing::CATALOG;
DropdownMenu::new(anchor, entries, open, dismiss_id, palette)
    .sizing(sizing)
    .width(sizing.fit_width(available_width))
    .render()
```

`.sizing(...)` configura ancho, densidad y presupuesto de filas. Llamar
después a `.width(...)` o `.max_visible_items(...)` permite un ajuste
justificado por el host. El constructor tradicional de `DropdownMenu`
conserva su geometría para las superficies que aún no usan estos perfiles.
`PermissionSelector` usa COMPACT de forma predeterminada.

El estándar de ayuda está en [Tooltips](ui-tooltip-standard.md).
