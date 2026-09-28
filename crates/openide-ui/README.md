# openide-ui

Motor de interfaz original para OpenIDE. La aplicación construye un árbol de
`Element`, `UiTree` calcula el layout y conserva el estado de controles por ID,
y el resultado es un `Frame` con comandos de dibujo y regiones interactivas.
El backend de ventana traduce esos comandos a su propio renderer.

## Contrato para el backend

1. Construir el árbol con `Element::{column,row,panel,overlay,popover,scroll,text,formatted_text,icon,button,icon_button,text_input,text_area,spacer}`.
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

## Controles compartidos

`Tabs`, `Popover` y `DropdownList` implementan `Component::render()` y devuelven
un `Element` ordinario. Reciben `ComponentPalette` del tema y admiten contenido
o acciones de cualquier vista. `Tab` y `DropdownOption` conservan los IDs de
acción del dominio; el host procesa los `UiAction::Clicked` como cualquier otro
botón. El estado `active` y `open` lo conserva el host entre reconstrucciones,
mientras `UiTree` retiene foco, hover y scroll por ID. `ControlSize` define
alturas de 24/28/32/36 px lógicos; `ControlState::Disabled` representa una opción
visual sin región interactiva.

`DropdownList` pinta cada opción como una única región de 36 px de alto con
icono de 16 px, etiqueta recortada y marca de selección al extremo derecho.
Reserva las mismas columnas para todas las filas, incluso cuando una opción
carece de icono; cambiar la selección no mueve el texto. El disparador puede
ser cualquier `Element` y `.placement(PopoverPlacement::BottomEnd)` alinea
el menú con su borde derecho. `SegmentedControl` mantiene el radio del track
en 9 px para acompañar los 7 px de los tabs y el inset de 2 px.
En listas que superan ocho opciones, flechas, Home/End y Tab desplazan el
`ScrollArea` para revelar la opción enfocada.

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
