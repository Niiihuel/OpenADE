# Componentes MIT de WarpUI aplicables a sesiones

Revisión: `references/warp` en `5af88f49f84e70025f9c19e13f6b9ae64b624627`.
Sólo se inspeccionaron `crates/warpui_core` y `crates/warpui`, declarados MIT.
Las rutas y líneas siguientes corresponden a ese clon. Esta es una guía de
implementación original para OpenIDE: los detalles de las pantallas de producto
de Warp no se pueden atribuir a estos dos crates. Los tamaños propuestos son
decisiones de OpenIDE, no constantes globales de Warp.

## Lo que hace robustos a los controles

| Mecanismo comprobado | Evidencia MIT | Aplicación concreta en sesiones |
| --- | --- | --- |
| Estado estable por control | `references/warp/crates/warpui_core/src/elements/gui/hoverable.rs:69-83,103-153,183-183,206-232` separa `MouseStateHandle` del hijo que se construye para pintar. `reset_interaction_state` evita dejar el estado presionado o el tooltip tras navegar. | Asignar identidad estable a fila, botón de menú y acción. Al cambiar de sesión o archivar, limpiar hover/pressed del control retirado. Selección y foco pertenecen al modelo de navegación, no a la instancia efímera del elemento. |
| Variantes y todos los estados | `references/warp/crates/warpui_core/src/ui_components/button.rs:78-110,129-155,263-285` define fondo, hover, pressed, disabled y activo, y resuelve su precedencia. `:140-143` desactiva el hit target cuando el botón está disabled. | `Nueva tarea`, buscar, atrás/adelante, fila, menú y botones de confirmación usan un mismo contrato visual. Disabled debe bloquear acción además de atenuarse. Hover, pressed, selected y foco no alteran padding ni geometría. |
| Composición de texto e icono | `references/warp/crates/warpui_core/src/ui_components/button.rs:15-39,295-335` permite icono antes o después del texto, restringe el icono a un tamaño y hace que el texto ocupe el espacio flexible. `:341-384` aplica borde, radio, padding, fondo y tamaño por capas. | El título de la sesión recibe el ancho flexible; estado, badge de CLI y botón `Más` conservan sus cajas. Una acción con texto e icono tiene una sola área clicable. No insertar espacios o caracteres Unicode para alinear un chevron. |
| Estilo parcial con precedencia | `references/warp/crates/warpui_core/src/ui_components/components.rs:54-94` define anchura, altura, fondo, borde, radio, fuente, padding y margen; `merge` aplica los valores del estado sobre el estilo base. | Definir variantes `sidebar_row`, `chrome_icon`, `menu_item`, `destructive`, `tab` en el framework; las vistas sólo eligen variante, contenido y acción. Evita colores/alturas dispersos en cada flujo. |
| Icono monocromo y DPI | `references/warp/crates/warpui_core/src/elements/gui/icon.rs:14-22,29-48,65-101` define una tinta/opacidad, rasteriza según escala física y centra el resultado en su caja. `references/warp/crates/warpui_core/src/ui_components/button.rs:295-316` usa el mismo color de estilo para texto e icono. | Mantener Solar Linear de una tinta con caja óptica de 16 px lógicos. Iconos de acciones dentro de hit targets de 28–32 px. Probar escalas 100, 150 y 200 %: el gris secundario depende del rol del control, nunca de un segundo color en el SVG. |
| Tooltip anclado | `references/warp/crates/warpui_core/src/ui_components/button.rs:247-260,387-435` posiciona el tooltip respecto del botón con separación de 8 px. `references/warp/crates/warpui_core/src/ui_components/tool_tip.rs:73-119` compone etiqueta y ayuda secundaria. | Botones de sólo icono requieren nombre y atajo visibles en tooltip; mostrar título completo de sesión si queda recortado. El tooltip no debe cambiar la medida de la fila. |
| Segmento con opción tipada | `references/warp/crates/warpui_core/src/ui_components/segmented_control.rs:22-40,66-87,123-180,187-225,298-352,386-395` conserva selección por opción, genera botón por opción, tooltip y evento tipado; deshabilitar quita el clic. | `Sesiones`/`Archivos` y filtros deben compartir el contrato de tab: estado seleccionado inequívoco, acción estable, posibilidad de disabled y tooltip. Si cambia la lista de opciones, preservar la opción válida o elegir explícitamente la primera. |
| Selección compacta | `references/warp/crates/warpui_core/src/ui_components/toggle_menu.rs:41-79,111-199` guarda selección fuera del render y diferencia selected/hovered. Es un selector horizontal, **no** un menú contextual completo. | Útil como referencia de tabs/filtros. Para `Más` de una sesión se necesita además overlay, lista vertical, navegación, cierre y retorno de foco propios. |
| Overlay real | `references/warp/crates/warpui_core/src/elements/gui/stack/mod.rs:140-173,177-224` pinta hijos posicionados sobre la UI, fuera del cálculo de tamaño; la capa overlay nace sin clip. | El menú de `Más`, el editor de título y la confirmación no empujan otras sesiones ni quedan cortados por el scroll. Anclar al botón, invertir arriba si falta espacio, cerrar con Escape/clic exterior y devolver foco al botón. |
| Texto con recorte explícito | `references/warp/crates/warpui_core/src/elements/gui/text.rs:254-274,298-300,566-570` separa wrapping y clip. `references/warp/crates/warpui_core/src/text_layout.rs:409-465,1482-1495` ofrece fade o elipsis y reserva ancho para el glifo de elipsis. | Títulos, proyecto y badge de CLI usan ancho medido, no límites por número de caracteres. Mantener siempre visible `Más`. Dar acceso al texto completo en tooltip o detalle. |
| Lista de altura fija | `references/warp/crates/warpui_core/src/elements/gui/uniform_list.rs:18-55,57-103,134-159,175-225` retiene scroll, pide el rango visible y permite desplazar el elemento activo a la vista. | Para filas de sesión de altura común, virtualizar la lista cuando crezca; el header, buscador y footer permanecen fijos. Selección por teclado debe llamar a `scroll_to` si sale del viewport. |
| Lista de altura variable | `references/warp/crates/warpui_core/src/elements/gui/viewported_list.rs:1-2,41-73,82-105,167-205,217-285` renderiza el rango visible, invalida alturas cambiadas y puede preservar posición. Su layout también puede medir elementos anteriores a un salto de scroll (`:313-331`), así que no garantiza costo estrictamente constante en todo caso. | Si las filas pasan a dos líneas o tienen subgrupos expandibles, usar un modelo de altura variable con ancla de scroll estable y medir saltos largos. No reconstruir y desplazar toda la lista al renombrar una tarea. |
| Resize y composición adaptable | `references/warp/crates/warpui_core/src/elements/gui/resizable.rs:14-38,44-81,101-164` separa tamaño persistente, límites y arrastre. `references/warp/crates/warpui_core/src/elements/gui/size_constraint_switch.rs:16-46,69-82,102-117` cambia estructura según restricciones. | El ancho del dock debe preservar el centro utilizable. En ancho estrecho, compactar metadata y chrome por reglas explícitas; no reducir el título a unos pocos caracteres mientras caben acciones superfluas. |
| Teclado como capa deliberada | `references/warp/crates/warpui_core/src/elements/gui/event_handler.rs:73-107,345-350` permite manejar keydown; el `Button` citado arriba se basa en `Hoverable` y no demuestra por sí solo una secuencia de foco/Tab/Enter/Espacio. | OpenIDE debe implementar y probar semántica de teclado para botón, fila, tab y menú; no asumir que el mecanismo de hover la aporta. Foco visible, flechas en listas/tabs, Enter/Espacio para activar y Escape para cerrar. |

## Contrato propuesto para la superficie de sesiones de OpenIDE

El contrato visual siguiente se aplica al dock izquierdo y al header central.
`docs/warp-visual-system.md` fija los tokens generales.

| Control | Caja y composición | Estados y comportamiento completo |
| --- | --- | --- |
| `Nueva tarea` | Fila de 32–36 px; icono de 16; etiqueta alineada al resto de navegación. | Hover, pressed, focus y busy coherentes. Clic/Enter crea o enfoca una tarea nueva una sola vez, la selecciona y mueve el foco al composer. Atajo mostrado en tooltip si existe. |
| `Buscar` | Misma geometría de navegación; atajo separado del label en un extremo. | Clic/Enter abre command center con foco en búsqueda; Escape vuelve al disparador. Búsqueda de tareas respeta proyecto, archivo y archivado según el filtro visible. |
| Plegar sidebar | Icon button de 32 px, icono de 16 y tooltip con `Ctrl+B`. | El dock izquierdo se pliega y aparece un botón para reabrirlo en el centro. La preferencia se guarda. El historial conserva sus atajos, sin botones visibles en el encabezado. |
| Tabs `Sesiones`/`Archivos` | Segmentos de 28–32 px de alto con igual baseline y área clicable. | Selección única y semántica; cambiar tab conserva scroll independiente. Flechas y Home/End recorren tabs, Enter/Espacio activa. |
| Fila de sesión | Una línea de título flexible; punto unread, CLI/estado y `Más` ocupan cajas reservadas. Altura objetivo 32 px, radio 6–8. | Base, hover, selected, selected+hover, unread, running, archived y focus visibles sin mover contenido. Clic activa sesión y marca lectura de modo explícito. Título completo accesible aunque esté recortado. |
| Botón `Más` | Hit target mínimo de 28 px; icono 16. Su área no solapa la activación de la fila. | Abre menú anclado, se mantiene visible y seleccionado durante el menú. Clic repetido/Escape/clic exterior cierra; foco regresa aquí. |
| Menú de sesión | Popover vertical: items de 32–36 px, iconos 16, separación y borde del token compartido. Debe caber dentro de ventana. | Cada acción tiene etiqueta y estado enabled. Flechas, Home/End y Enter operan la lista; el menú no desplaza filas. `Renombrar`, `Fijar/Desfijar`, `Archivar` o `Restaurar`, y `Eliminar` corresponden al estado de la tarea. |
| Renombrar | Campo con título actual seleccionado y acciones `Guardar`/`Cancelar`; no tapar contexto de la tarea. | Enter guarda título válido, Escape cancela; error visible si vacío o si falla persistencia. Conservar selección, scroll y foco tras guardar/cancelar. |
| Eliminación | Confirmación con nombre de tarea y acción destructiva separada del menú ordinario. | Sólo se elimina al confirmar. Escape/Cancelar no mutan datos. Tras borrar, seleccionar vecino válido o home; mostrar fallo si almacenamiento falla. |
| Grupo proyecto, fijadas y archivo | Cabeceras legibles pero discretas; filas idénticas entre grupos. | Agrupar por identidad real del proyecto, no sólo basename; ordenar estable; archivar/fijar preserva la posición razonable del scroll. Archivo vacío y error de carga tienen estados distintos. |
| Header de sesión | Título flexible; `Nueva tarea`, panel derecho y ajustes en cajas fijas. Metadata en segunda línea sólo si cabe. | Título de longitud arbitraria sin invadir acciones; controles ocultos por breakpoint reaparecen al ensanchar. Running y backend reflejan estado real. |

### Brechas visibles en el código actual

Estas observaciones son de OpenIDE, no de Warp. Al momento de escribir esta
guía, `apps/agents-window/src/view.rs:323-408` construye fila con punto unread,
botón de título, badge CLI y botón `Más`; es una buena separación de cajas, pero
el ancho del título resta una cifra fija (`:341-342`) y el badge recorta por
caracteres (`:331-340`). El menú y los formularios se construyen aparte en
`:410-539`; hay que verificar retorno de foco, scroll y errores de persistencia
en cada acción. El header central en
`apps/agents-window/src/session_surface.rs:117-136` calcula elipsis por número
estimado de caracteres; conviene cambiarlo a recorte por anchura medida y reservar
primero las cajas de acciones.

`apps/agents-window/src/view.rs:578-821` ya separa header, tabs, scroll y footer.
La lista de sesiones se construye completa dentro del scroll (`:632-713,814`);
para listas grandes, el camino de WarpUI es un viewport de altura uniforme o
variable según la geometría real. El objetivo no es copiar su implementación,
sino respetar la misma garantía: trabajo proporcional a las filas visibles y
scroll anclado.

## Checklist de aceptación visual y funcional

- Capturar `Nueva tarea`, fila, tab, icon button y menu item en **base, hover,
  pressed, selected, focus, disabled y busy** cuando aplique. Los rectángulos,
  baseline y padding deben coincidir entre estados.
- Comparar dock de sesiones a 900, 1200 y 1600 px de ancho, y 100, 150 y 200 %
  de escala. Probar título de 100 caracteres, emoji, tildes y ruta de proyecto
  larga; iconos de 16 px nítidos y alineados.
- Recorrer sólo con teclado: nueva tarea → búsqueda → tabs → grupos → filas →
  `Más` → todas las acciones → confirmación → Escape. El foco siempre se puede
  localizar y no queda detrás de un popover.
- Probar cada acción y su efecto persistido: activar, renombrar, fijar,
  desfijar, archivar, restaurar, cancelar y eliminar. Volver a abrir la app y
  comprobar selección, orden y archivo. En fallos de disco, mantener estado
  anterior y mostrar error sin cerrar la tarea incorrecta.
- Probar 1.000 sesiones, alternar proyectos y tabs y abrir menús cerca de los
  cuatro bordes. No debe saltar el scroll ni cortarse el menú; layout y pintura
  deben depender de los ítems visibles.
- Verificar que tooltips, hover y pressed desaparezcan al navegar, que el
  splitter no corte el centro y que mouse wheel desplaza sólo la región bajo
  el puntero.

## Límite de licencia

`references/warp/README.md:52-56`,
`references/warp/crates/warpui_core/Cargo.toml:7` y
`references/warp/crates/warpui/Cargo.toml:7` declaran MIT sólo para esos crates.
Esta guía resume mecanismos; no incorpora código ni assets de Warp ni trata
las vistas de producto AGPL como material MIT.
