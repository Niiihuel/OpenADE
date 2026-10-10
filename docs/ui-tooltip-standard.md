# Tooltips de la ventana nativa

El componente compartido es `openide_ui::Tooltip` (`crates/openide-ui/src/components.rs`). Envuelve el control existente con su ID de acción: muestra la misma superficie flotante y, si corresponde, el atajo con `Tooltip::shortcut`. No agrega área de clic ni altera la medida del control. `UiTree` lo presenta tras hover o inmediatamente con foco de teclado; lo coloca dentro del viewport y fuera del clip de los scrolls. Para `Button` e `IconButton`, también se puede usar `.tooltip(...)` y `.tooltip_shortcut(...)`.

Los tooltips usan esquinas de **6 px**, iguales a las superficies de los
popovers y menús. Sus controles de origen usan **4 px**: el hover, la pulsación
y el foco conservan exactamente el área de interacción, sin convertirse en
píldoras ni invadir la acción vecina. Las teclas mantienen su propia geometría
compartida de 20 px de alto y radio de 4 px.

## Posición según la superficie

La colocación se elige en el control que conoce su contexto; no se cambia el
valor por defecto de todos los tooltips a un mismo lado.

- Barra de actividad izquierda del IDE, ajustes de esa barra y acciones del
  extremo derecho de las filas de tareas en Agents: `Right`, hacia el área de trabajo.
- Metadatos del explorador: `Right` en el panel izquierdo del IDE y `Left` en
  el dock derecho de Agents. No cubren la fila vecina de la lista.
- Cabeceras superiores, pestañas del dock, navegador y botones de búsqueda:
  `Below`, por debajo de su barra.
- Barra de estado y controles inferiores del composer: `Above`.
- Cabecera de terminal y panel inferior: `Above` en tamaño normal; `Below`
  al maximizar el panel, cuando su cabecera pasa al borde superior.

`Tooltip::placement`, `Button::tooltip_placement` y
`Element::tooltip_placement` (para un tooltip ya renderizado por un helper)
comparten esta política. No agregan envoltorios ni cambian el ID del control.
El motor intenta el lado solicitado, luego el opuesto y, si ninguno cabe,
el otro eje antes de ajustar al viewport. Mantiene 8 px de separación cuando
hay espacio para evitar cubrir el disparador.

Referencias locales consultadas: VS Code `activitybarPart.ts` (lado interior),
`sidebarPart.ts` y `auxiliaryBarPart.ts` (cabeceras), `panelPart.ts` (normal y
maximizado), `titlebarPart.ts` (debajo). Zed `crates/gpui/src/window.rs` ajusta
el tooltip a partir del puntero y cambia de lado en los bordes; aquí se conserva
el anclaje al control para que teclado y ratón usen la misma posición.

## Información en secciones

Los menús compactos muestran etiquetas; sus explicaciones aparecen en
tooltips al costado, con un máximo de 320 px. Las medidas compartidas de
filas y popovers se documentan en [Selectores y popovers](ui-selector-standard.md).

Para ayuda extensa, `Tooltip::description` agrega párrafos, `Tooltip::separator`
inserta un divisor y `Tooltip::footer` muestra información secundaria. El ancho
se configura con `max_width`; el motor mide cada línea y calcula la altura,
respetando los límites del viewport. Los tooltips simples conservan su superficie
compacta. Los bloques son informativos y no agregan acciones ni foco.

El selector de permisos del composer usa el nombre del modo como título y la
explicación en `description`, con un ancho máximo de 320 px. Durante un turno
sigue mostrando toda la ayuda al pasar el cursor por el control deshabilitado;
las instrucciones no se truncan como una etiqueta de una sola línea.

`Tooltip::icon_row` agrega una fila de `TooltipItem` con iconos y etiquetas
cortas. El motor alinea las etiquetas de las filas y distribuye los items en
varias líneas si falta ancho. Los iconos usan `ICON_SIZE` (16 px); la fila
también puede mostrar un item de texto sin icono, por ejemplo «Dato no
disponible». Los items conservan el carácter pasivo del tooltip.

```rust
Tooltip::new(anchor, action_id, "Nombre del modelo", palette)
    .description("Proveedor · ID exacto")
    .separator()
    .icon_row("Entrada", [
        TooltipItem::new("Texto").icon(Icon::Text),
        TooltipItem::new("Imagen").icon(Icon::Image),
    ])
    .description("Información sobre las capacidades de este modelo.")
    .separator()
    .footer("Autenticación administrada por el proveedor.")
    .max_width(380.0)
    .placement(TooltipPlacement::Left)
    .render()
```

`TooltipPlacement::Left` y `Right` permiten presentar la ayuda al costado. El
motor cambia de lado y ajusta el ancho según el espacio disponible. Para una
fila dentro de un popover, usa el borde del menú completo como referencia
horizontal y la fila como referencia vertical, para mantener visible la lista.
Si la ventana no deja espacio lateral suficiente, vuelve a colocar la ayuda
arriba o abajo de la fila, manteniendo el texto accesible.

El selector de modelos usa esta estructura para separar identidad, capacidades,
límites, tarifas y autenticación. Los textos multilínea también se miden por
línea, evitando que se dibujen sobre una cápsula con altura fija.

## Inventario de uso

| Superficie | Acciones con tooltip compartido |
| --- | --- |
| Sidebar y navegación | Plegar/reabrir, ajustes, orden, crear grupo, archivadas y menús de tarea/grupo. |
| Composer | Proyecto, modo, permisos, adjuntos, modelo/esfuerzo, enviar/detener y chips de archivos. |
| Búsqueda global | Acción «Cerrar» con `Esc` en el pie de la paleta. |
| Dock derecho | Abrir pestaña, elegir entre todas las tabs, recorrer la barra horizontal, refrescar archivos y Git, overview, ocultar panel, cerrar tabs y preview. |
| Marketplace | Actualizar catálogo, cerrar formulario, desinstalar skill, quitar origen y activar/desactivar skill. |
| Automatizaciones | Editar, ver historial, ejecutar ahora, eliminar y activar/pausar tarea; elegir plantilla. |

Los botones con texto visible y las filas de menú conservan su etiqueta. Los menús, tooltips y buscadores reutilizan `Kbd` con las mismas teclas con borde. Los tooltips se reservan para ayuda contextual, acciones sólo con icono o texto truncado. Cuando un menú está abierto, su disparador puede omitir el tooltip para que la ayuda no cubra el popover; el mismo disparador lo muestra mientras el menú está cerrado.

## Apariencia de las teclas

`Kbd`, `Tooltip::shortcut`, los menús y los buscadores comparten el mismo
pintor. La adaptación de OpenIDE usa teclas de **20 px** de alto, ancho mínimo
de **22 px**, relleno horizontal de **5 px**, texto Sans de **11 px** y esquinas
de **4 px**. El fondo y el borde son sutiles; el borde inferior de **1 px**
conserva el relieve sin duplicar el contorno. Estas medidas son del sistema
visual de OpenIDE, no medidas idénticas a las de VS Code.
Las combinaciones incluyen el separador `+`; una secuencia de combinaciones
separa cada acorde con espacio. `Ctrl+K Ctrl+C` conserva dos acordes distintos,
`Ctrl++` conserva la tecla `+` y `Ctrl+-` conserva la tecla `-`.
Las flechas de navegación se dibujan con los iconos vectoriales compartidos
para no depender de que la fuente incluya esos glifos.

Los menús de acciones y contextuales usan `Kbd::new(...)`, alineado a la
derecha, con las mismas cápsulas que el tooltip del buscador. El ancho se mide
con bordes, rellenos y separadores incluidos. Las filas reservan primero el
texto de la acción y ajustan su altura cuando necesita varias líneas. En macOS,
Control se representa con `⌃` y Command con `⌘`; ambos son modificadores
distintos. Los grupos intermedios se separan con un divisor, sin duplicar uno
que ya esté declarado por el host.

Los atajos no agregan clics, foco ni selección de texto. Su ancho se calcula
con las medidas reales de la fuente. Cuando el espacio es insuficiente, se
omite el grupo completo; nunca se muestra un atajo truncado ni una cápsula
vacía. Los menús reservan espacio para la etiqueta de la acción y los buscadores
para el icono y el texto antes de incluir la pista.

La referencia se revisó en el checkout local de VS Code,
`/home/nihuel/projects/personal/refs/vscode/`: el componente
[`keybindingLabel`](https://github.com/microsoft/vscode/blob/main/src/vs/base/browser/ui/keybindingLabel/keybindingLabel.css)
y su [`renderizado`](https://github.com/microsoft/vscode/blob/main/src/vs/base/browser/ui/keybindingLabel/keybindingLabel.ts)
definen una tecla con borde, relleno y separadores entre combinaciones. En
`references/zed/crates/ui/src/components/keybinding.rs`, Zed centraliza la
presentación por plataforma y resuelve la combinación según la acción y el
foco; `tooltip.rs` reutiliza ese componente junto al título de la ayuda.
OpenIDE toma esta reutilización y la legibilidad de las teclas de VS Code como
referencia, adaptando las medidas a su propia UI. Las mismas teclas aparecen
en tooltips, menús y ajustes, en componentes nativos sin DOM ni estilos web.

## Paleta de búsqueda y acciones

El buscador ocupa todo el ancho disponible de la cabecera. Debajo, las
categorías «Todo», «Acciones», «Tareas» y «Archivos» usan el `SegmentedControl`
compartido, alineado al inicio del buscador, con el texto centrado dentro de
cada segmento y una selección claramente visible. Los resultados mantienen
su área desplazable independiente.

La acción «Cerrar» queda en el pie junto a la tecla `Esc`, separada del campo
de entrada. El pie también presenta las pistas de navegación y apertura con
el mismo `Kbd`; las flechas son vectoriales. El cierre por teclado conserva
su comportamiento aunque el foco previo estuviera en la terminal.

Las pruebas de `shortcut`, `components`, `menu` y `dropdown_menu` verifican
las combinaciones completas, los símbolos literales, las secuencias, el
centrado vertical y la omisión del grupo cuando falta espacio.

Al agregar una acción sólo con icono, usar `Tooltip::new(anchor, id, título, palette)` o las opciones de tooltip del botón compartido. El `id` del tooltip debe coincidir con el ID del control para conservar hover, foco y activación.
