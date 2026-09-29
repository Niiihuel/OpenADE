# openide-ui

Motor de interfaz original para OpenIDE. La aplicación construye un árbol de
`Element`, `UiTree` calcula el layout y conserva el estado de controles por ID,
y el resultado es un `Frame` con comandos de dibujo y regiones interactivas.
El backend de ventana traduce esos comandos a su propio renderer.

## Contrato para el backend

1. Construir el árbol con `Element::{column,row,panel,overlay,popover,scroll,text,formatted_text,icon,button,icon_button,text_input,text_area,skeleton,spinner,spacer}`.
2. Conservar una instancia de `UiTree` y llamar `set_root` al actualizar el árbol.
   Los controles con el mismo ID conservan foco, cursor, selección y scroll.
3. Llamar `layout(Size)` antes de entregar eventos de puntero y otra vez antes
   de pintar. Los eventos producen `UiAction` para botones y entradas de texto.
4. Dibujar `Frame::commands` en orden. `PushClip` y `PopClip` delimitan el recorte
   de scroll e input; el renderer debe intersectar clips anidados.

`FontMetrics` y la fuente elegida por el backend deben coincidir. El tamaño
predeterminado es 14 px lógicos y la familia predeterminada es monoespaciada.
`Element::font_family(FontFamily::Sans)` elige la fuente de UI;
`Element::font_size(...)` ajusta su tamaño. El host calibra la fuente mono con
`set_metrics` y la fuente sans con `set_sans_metrics`, ambas a 14 px. Para
ajustar líneas con las mismas métricas que el renderer, el host registra
`UiTree::set_text_measurer(|texto, tamaño, familia| ancho)`. Si no lo hace,
el motor usa las métricas calibradas como alternativa. `formatted_text`
recibe `TextSpan` con color, tamaño, familia y fondo opcional por segmento;
sus saltos se calculan con el medidor del host y respetan los grafemas Unicode.
Los segmentos se pintan como comandos `Text` normales y pueden seleccionarse
o copiarse con Ctrl+C; los enlaces todavía no se activan. `text_input` es de una
línea; `text_area` permite varias, recorta y desplaza su contenido con la
rueda. El backend aplica el factor de escala de pantalla.

`Element::border(color, width)` crea un trazo interno; `radius` se aplica tanto
al fondo como al borde. El trazo no modifica el tamaño calculado del elemento.

`Element::icon(Icon::...)` crea un icono semántico y
`Element::icon_button(id, Icon::..., label)` crea un botón interactivo con icono.
Una etiqueta vacía produce un botón de sólo icono. Todos los iconos ocupan
16 px lógicos; `icon_size` se conserva temporalmente por compatibilidad y no
altera ese tamaño. `foreground(color)` aplica una sola tinta semántica.
El motor emite `DrawCommand::Icon`; el backend elige el arte. En Agents Window,
el backend usa un conjunto pequeño de Solar Icons Linear incluido en
`apps/agents-window/assets/solar/`, con sus licencias y atribución.

La navegación con `Key::Tab` y Shift+Tab recorre botones e inputs en orden de
árbol y conserva el foco al reconstruir controles con los mismos IDs. Enter y
Space activan un botón enfocado; en un input, Enter emite `Submitted` y los
espacios llegan como `UiEvent::Text(" ")`.

En `text_area`, Enter inserta un salto de línea y Ctrl+Enter emite `Submitted`.
Shift+flechas selecciona texto, Ctrl+A selecciona todo, y las flechas arriba y
abajo navegan por las líneas visibles. `input_value` y `set_input_value`
funcionan igual para ambos tipos de editor.

Los botones interpolan sus fondos de hover y pulsación durante 120 ms por
defecto. `transition_ms(0)` fuerza una respuesta inmediata;
`UiTree::set_reduced_motion(true)` desactiva las transiciones. El host debe
pedir un repintado corto mientras `UiTree::is_animating()` sea verdadero.
`ScalarTransition` anima valores escalares (ancho, posición u opacidad) con
`retarget(value, now, duration)` sin saltos a mitad del movimiento;
`retarget_with_motion(..., reduced_motion)` y `snap_to(...)` resuelven cambios
instantáneos. `Easing::OutCubic` y `Easing::InOutCubic` ofrecen curvas breves y
suaves. La aplicación conserva esta transición entre reconstrucciones y usa
`is_active(now)` para programar repintados.

`Tabs` mueve el fondo de selección entre controles con el mismo `Tabs::id()`;
si no se indica un ID, utiliza el primer ID de acción. `animation_ms(0)` hace
el cambio instantáneo. `Popover` aplica una entrada de 140 ms y una salida de
hasta 110 ms a la pintura (opacidad y desplazamiento de 4 px). Al cerrar,
retira de inmediato las regiones de clic y foco; únicamente persiste la pintura
durante la salida. Si se reabre mientras sale, continúa desde la opacidad
visible. `Popover::animation_ms(0)` desactiva ambos movimientos.
Ambos respetan `UiTree::set_reduced_motion(true)`.

`Element::clip_children()` recorta pintura y regiones interactivas de los
descendientes al contenido del contenedor. El fondo, borde y sombra del
contenedor se dibujan completos. El clip es rectangular y permite contraer
un dock sin que su contenido ancho invada el panel vecino.
`UiTree::focus(id)` enfoca un control existente después de `layout`, útil para
abrir un buscador listo para escribir.

El motor no depende de Warp ni de sus recursos. Toma como referencia la
separación conceptual entre estado, árbol visual, layout, escena y renderer que
Warp documenta para su framework de UI.

`Skeleton` representa contenido en carga sin agregar un objetivo de clic. Su
pulso de color se calcula en `motion.rs`, conserva la fase por ID entre cambios
del árbol y no cambia la geometría. El host sigue usando `is_animating()` para
pedir repintados; al quitar los Skeletons del árbol, deja de pedirlos. Con
`set_reduced_motion(true)` el bloque queda estático.

```rust
use openide_ui::{Component, ComponentPalette, Skeleton};

let placeholder = Skeleton::new("catalog-title", ComponentPalette::neutral_dark())
    .width(180.0)
    .height(12.0)
    .radius(6.0)
    .render();
```

`Spinner` indica una operación sin duración conocida. Su forma predeterminada
es un aro incompleto que gira continuamente; `SpinnerVariant::Spokes` ofrece la
variante de radios. El backend dibuja ambas con trazos vectoriales. Conserva la
fase por ID, no crea una región interactiva y se detiene en una posición fija
con `UiTree::set_reduced_motion(true)`.

```rust
use openide_ui::{Color, Component, Spinner, SpinnerVariant};

let progress = Spinner::new("search-progress", Color::WHITE)
    .size(28.0)
    .variant(SpinnerVariant::Circle)
    .render();
```

`Modal` sirve para formularios que deben quedar centrados sobre la ventana. Su
`body` es desplazable cuando se fija `height`, con la barra reservada fuera del
ancho del formulario; la cabecera y el pie permanecen visibles.
`initial_focus` elige el primer campo; Escape y la X gruesa sin círculo
despachan el mismo ID de cierre, y el foco vuelve al control anterior al cerrar.
El fondo queda bloqueado y `backdrop_dismiss(true)` permite cerrar al pulsarlo
cuando el flujo admite descartarlo.

`Stepper` muestra pasos completados, activo y pendientes en una barra compacta.
Recibe el paso actual del host y emite los IDs de los pasos ya alcanzados;
los pendientes no se pueden pulsar para saltar validaciones. El host muestra
un solo formulario por paso, conserva los valores al volver y decide si
`Siguiente` puede avanzar. Automatizaciones lo usa para Contenido, Horario y
Agente dentro del `Modal`, con un único scroll exterior al formulario.

`DatePicker` ofrece entrada ISO y calendario mensual navegable; `TimePicker`
ofrece entrada `HH:MM` y ajustes de hora/minuto. `DateTimePicker` los combina
sin unir sus estados, para conservar lo escrito en cada campo mientras se
edita el otro. El host procesa los IDs de navegación y selección que documenta
cada componente y valida los valores antes de guardar.

## Controles compartidos

`Button` es la acción compartida de la aplicación. `ButtonContent` distingue
etiqueta, icono y ambos; `IconButton` conserva la API breve para acciones de
icono. `ControlSize::button_sizing()` centraliza alto, tipografía, espaciado y
radio. El tema llega mediante `ComponentPalette` y `ButtonVariant`; `disabled`
quita la región de clic y foco. `.tooltip(...)` y
`.tooltip_shortcut(...)` asocian una ayuda al mismo ID de acción. El atajo se
muestra en la ayuda; la activación por teclado sigue el enrutamiento de
`UiTree` y la aplicación interpreta `UiAction::Clicked`.

`Empty` ofrece un estado vacío centrado con icono, título, descripción y una o
varias acciones `Button`. Mantiene una altura legible aun si el contenedor pide
menos espacio: 256 px, o 228 px con `.compact()`. `.height(...)` permite darle
más espacio, por ejemplo para centrarlo en el viewport. `.action_element(...)`
admite un control propio en la misma fila centrada. Por ejemplo:

```rust
use openide_ui::{Button, ButtonVariant, Component, ComponentPalette, Empty, Icon};

let palette = ComponentPalette::neutral_dark();
let retry = Button::new("retry", "Reintentar", palette)
    .variant(ButtonVariant::Secondary)
    .render();
let empty = Empty::new("Sin resultados", "Prueba otro término.", palette)
    .icon(Icon::Search)
    .compact()
    .height(320.0)
    .action_element(retry)
    .render();
```

El borde discontinuo y la versión compacta sirven para páginas y paneles.
`Kbd::new("Ctrl+Shift+M", palette)` muestra el atajo
completo en una cápsula. Úsalo también en filas y listas; `Tooltip::shortcut`,
`Search::shortcut`, `DropdownMenuItem::shortcut` y `MenuItem::shortcut` aplican
el mismo tratamiento. Para un estado como “Abierta”, usa
`DropdownMenuItem::annotation` y conserva el texto sin apariencia de tecla.
`Search` (también disponible como `SearchField`) permite agregar una pista con
`.shortcut("Ctrl+K")` dentro del único objetivo de foco del campo. Todas las
instancias de `Tabs` comparten el carril redondeado del selector de
Grupo/Proyecto, incluido su indicador animado.

`TextField` y `TextArea` aplican la misma paleta, borde de foco y densidad a los
formularios. Ambos conservan el valor por ID y dejan de emitir acciones de
entrada al usar `.disabled()` o `.enabled(false)`; el host sigue manejando
`UiAction::InputChanged`.

Las variantes visuales son `Default`, `Secondary`, `Outline`, `Ghost`,
`Destructive`, `Link`, `Warning` y `Success`. `Link` se subraya al recibir hover
o foco de teclado. `ButtonSize` ofrece `Default` (36 px), `Small` (32 px),
`Large` (40 px), `Icon` (36 px), `IconSmall` (32 px) e `IconExtraSmall` (24 px)
mediante `.button_size(...)`. `Subtle`, `Raised` y `Solid`, junto con
`.size(ControlSize)`, siguen disponibles para los controles existentes.

```rust
use openide_ui::{Button, ButtonContent, ButtonVariant, Component,
                 ComponentPalette, Icon};

let action = Button::with_content(
    "project:open",
    ButtonContent::IconAndLabel(Icon::Folder, "Abrir proyecto".into()),
    ComponentPalette::neutral_dark(),
)
.variant(ButtonVariant::Raised)
.tooltip("Abrir proyecto")
.tooltip_shortcut("Ctrl+O")
.render();
```

`Tabs`, `Popover` y `DropdownList` implementan `Component::render()` y devuelven
un `Element` ordinario. Reciben `ComponentPalette` del tema y admiten contenido
o acciones de cualquier vista. `Tab` y `DropdownOption` conservan los IDs de
acción del dominio; el host procesa los `UiAction::Clicked` como cualquier otro
botón. El estado `active` y `open` lo conserva el host entre reconstrucciones,
mientras `UiTree` retiene foco, hover y scroll por ID. `ControlSize` define
alturas de 24/28/32/36 px lógicos; `ControlState::Disabled` representa una opción
visual sin región interactiva.

`DropdownMenu` reúne acciones, encabezados y separadores en un popover con un
único ID de cierre. Cada `DropdownMenuItem` puede tener icono, atajo visible,
selección o estado deshabilitado. `DropdownList` adapta sus opciones al mismo
componente para compartir columnas, navegación y movimiento. `MenuMotion`
controla la entrada del popover y la transición de hover/pulsación de cada
fila; `UiTree::set_reduced_motion(true)` los muestra sin animación. El host
mantiene el estado abierto y procesa los IDs de las acciones.
El popover pinta una sola superficie; las filas normales son transparentes y
solo la fila bajo el puntero o seleccionada recibe un fondo. Así el fundido de
entrada y salida no superpone capas de color en todas las filas.
Una fila puede declarar `.submenu(vec![...])`; el panel secundario aparece al
pasar el puntero o enfocar la fila, se coloca a la derecha y cambia a la
izquierda si falta espacio. Flecha derecha entra al submenú y flecha izquierda
regresa a su fila. Encabezados y separadores también funcionan dentro de él.
`DropdownMenuEntry::Custom` permite incluir controles propios cuando hacen falta;
las acciones destructivas pueden
usar `.destructive(color)`.

`AlertDialog` muestra una superficie modal centrada con título, descripción,
cancelación, cierre y acción principal. El fondo atenuado bloquea acciones de
la pantalla; Escape cancela y un clic fuera no cierra el diálogo. El foco queda dentro
del diálogo y vuelve al control anterior al cerrarlo. El host conserva el
estado abierto y ejecuta la operación sólo tras recibir el ID de confirmación.
`.body(element)` agrega un campo u otro contenido entre la descripción y el
pie; el renombrado de tareas y grupos usa esta forma en lugar de un menú lateral.

`Composer` ofrece un editor multilínea controlado y espacios para contexto,
adjuntos y pie. El host suministra el texto, alto, ancho y colores en cada
reconstrucción; `UiTree` conserva el foco y el cursor por ID. Esta separación
permite usar la misma tarjeta en inicio y conversación sin trasladar la lógica
de modelos, permisos o envío al motor de UI.

`DropdownList` pinta cada opción como una única región de 36 px de alto con
icono de 16 px, etiqueta recortada y marca de selección al extremo derecho.
Reserva las mismas columnas para todas las filas, incluso cuando una opción
carece de icono; cambiar la selección no mueve el texto. El disparador puede
ser cualquier `Element` y `.placement(PopoverPlacement::BottomEnd)` alinea
el menú con su borde derecho. `SegmentedControl` mantiene el radio del track
en 9 px para acompañar los 7 px de los tabs y el inset de 2 px.
En listas que superan ocho opciones, flechas, Home/End y Tab desplazan el
`ScrollArea` para revelar la opción enfocada.

`ScrollArea::uniform_list` construye solo las filas visibles; la lista de
tareas y el árbol de archivos lo usan para mantener estable el trabajo por
cuadro. Las áreas anidadas reparten un gesto de rueda: al llegar un panel a su
límite, el desplazamiento restante pasa al contenedor exterior en el mismo
evento. Los deltas no finitos se descartan y la posición conservada por ID se
limita al contenido actual.

```rust
use openide_ui::{Component, ComponentPalette, DropdownList, DropdownOption,
                  Element, Icon, Popover, PopoverPlacement, Tab, Tabs};

let palette = ComponentPalette::neutral_dark();
let tabs = Tabs::new(vec![
    Tab::new("view:sessions", "Sesiones", 110.0).icon(Icon::Chat).active(true),
    Tab::new("view:files", "Archivos", 110.0).icon(Icon::Folder),
], palette).render();

let picker = DropdownList::new(
    Element::button("agent:toggle", "Agente"),
    vec![DropdownOption::new("agent:codex", "Codex").selected(true)],
    true, "agent:dismiss", palette,
).width(220.0).render();

let menu = Popover::new(tabs, picker, true, "menu:dismiss", palette)
    .placement(PopoverPlacement::BottomEnd)
    .render();
```

`Element::popover` ancla el contenido flotante al rectángulo real del control,
lo coloca sobre el resto de la escena, escapa del recorte de listas con scroll,
se adapta al borde de la ventana y cambia de lado vertical cuando no cabe. Un
clic fuera emite `Clicked` con el ID de cierre; Escape emite la misma acción.
La superficie flotante bloquea el cierre cuando se pulsa en un espacio vacío
dentro del menú. El backend debe pasar Escape como `UiEvent::Key` y repintar
después de actualizar el estado `open`.

Los rótulos de `Button` e `IconButton` se recortan con `truncate_measured`
usando el medidor de texto del renderer. El recorte conserva grafemas Unicode
completos y se dibuja dentro del control sin alterar su región interactiva.
