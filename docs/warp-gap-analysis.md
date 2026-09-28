# Brechas de calidad visual frente a WarpUI

Auditoría del clon local `references/warp` frente al framework original
`crates/openide-ui` y al host `apps/agents-window`. Las capturas de Warp
aportadas por el usuario muestran una ventana densa con pestañas y búsqueda
integradas, conversación y editor en el centro, revisión de código a la derecha,
controles de estado en el borde inferior y capas flotantes que no desplazan el
contenido. ZCode sigue definiendo la estructura de producto; WarpUI sirve aquí
para entender los mecanismos que hacen viable ese acabado en Rust.

## Matriz técnica

| Sistema | Evidencia en WarpUI | Estado comprobado en OpenIDE | Brecha y efecto observable |
| --- | --- | --- | --- |
| Elementos y controles | `warpui_core/src/elements/gui/mod.rs` registra contenedores, alineación, clips, stacks, drag, resize, listas, tabla y texto formateado; `src/ui_components/mod.rs` registra botones, switches, menús, tooltips, chips, sliders y más. | `openide-ui/src/element.rs` ofrece fila, columna, panel, scroll, texto, icono, botón e inputs; `view.rs` compone los docks. | Falta un vocabulario reutilizable para menú, popover, tooltip, badge, tab y estado deshabilitado. Las vistas deben reconstruirlos manualmente y divergen en altura, padding y respuesta. |
| Layout y límites | `elements/gui/flex/mod.rs` tiene alineaciones en ambos ejes, orientación, ajuste y flex; `constrained_box.rs`, `align.rs`, `stack/mod.rs` y `resizable.rs` separan medidas, posicionamiento y superposición. | `tree.rs:837-945,1299-1374` mide filas/columnas con flex simple. Los anchos de dock se calculan en `view.rs:264-314`. | Falta ancho máximo centrado para lectura, alineaciones generales y restricciones expresadas por elemento. El contenido largo y el composer aún dependen de cálculos específicos de la ventana. |
| Estado y actualización | `warpui_core/README.md` describe `App`, entidades, handles, vistas efímeras y contexto; `core/app.rs` ofrece observación y actualizaciones de vistas/modelos. | `UiTree` conserva foco, texto, scroll y hover por ID (`tree.rs:372-513`). `AgentsWindow` reconstruye el árbol desde `ViewState`. | El estado local básico ya existe. Falta invalidación por subárbol y observación granular: sesiones largas generan medición completa por frame y dificultan animaciones de paneles con estado propio. |
| Tipografía | `text_layout.rs:62-265` tiene cachés de línea/frame y disposición por estilo; `warpui/src/windowing/winit/fonts/text_layout.rs` maneja glifos, fuentes y runs; `FormattedTextElement` usa familias, headings, enlaces y selección. | `UiTree::set_text_measurer` recibe el ancho real de egui para párrafos, botones, inputs y `TextSpan`; `wrap_measured` preserva grafemas y evita medir todo un párrafo largo por cada línea. Hay roles de tipo semánticos y `Element::formatted_text`. | Quedan selección/copia de respuestas, enlaces activables, peso/itálica reales y caché de layout por subárbol. |
| Animación | `elements/animation.rs` modela tiempo y fotogramas; `elements/gui/shimmering_text.rs` y `scene.rs:78-96` permiten efectos de glifos; los controles conservan estados visuales. | `motion.rs` y `tree.rs:950-977` interpolan hover/pulsación por ID durante 120 ms y respetan movimiento reducido. | Ya hay microinteracción útil. Faltan transiciones de apertura/cierre, selección y atención entre docks, tooltips, cards y scroll; deben animar pintura/posición sin bloquear las acciones. |
| Capas y composición | `scene.rs:18-64,358-514` separa capas normales y overlay, clip por capa, hit map, click-through y orden Z; `stack/mod.rs:145-171` agrega hijos flotantes sin clip del panel padre. | `Element::overlay` pinta al final de la escena y gana el hit test sin heredar el clip del scroll; `DrawCommand::Shadow` eleva superficies sin aumentar su área clicable. Command Center aún usa una segunda `UiTree` desde el host. | Faltan foco modal restringido, cierre exterior y restauración de foco como contratos del framework; los menús y tooltips todavía no usan esta capa. |
| GPU y efectos | `warpui/src/rendering/wgpu/renderer.rs` coordina la escena; `renderer/frame.rs:119-174` aplica scissor por capa; `renderer/rect.rs:98-139` prepara sombras y radios; `renderer/glyph.rs` y `atlas/manager.rs` cachean glifos/texturas. | `render.rs` traduce comandos a egui/wgpu; cachea SVG rasterizados por icono, tamaño DPI y paleta (`render.rs:12-114`), y ya pinta radios, clips y `Shadow` (`render.rs:161-270`). | La calidad visible puede subir con la escena actual. La mayor brecha inmediata es de texto y composición. Un backend wgpu propio completo sería una fase posterior, guiada por perfiles de rendimiento y paridad de escena. |
| Input, foco y scroll | `elements/gui/new_scrollable/mod.rs` distingue ejes y scrollbars; `core/app.rs` mantiene acciones y observadores; `ui_components/button.rs:78-95` separa estados normal, hover, activo y deshabilitado. | `UiTree` conserva foco, selección de inputs y scroll vertical; Tab y Shift+Tab recorren controles. El scroll muestra thumb proporcional, admite arrastre y clic en pista, y conserva posición por ID. | Faltan scroll horizontal/dual, inercia, selección de texto estático, deshabilitado semántico y navegación espacial consistente en pestañas/listas. |
| Listas largas | `elements/gui/uniform_list.rs:57-103,205-214` construye sólo rango visible; `viewported_list.rs:1-2,169-175,217-258` soporta alturas variables y preservación de scroll tras invalidaciones. | `Element::scroll` mide al hijo completo y recorta al pintar (`tree.rs:1007-1030`); sesiones y timeline se representan como todos sus elementos. | Con cientos de tareas, archivos o eventos, reconstruir y medir toda la lista escala linealmente. La virtualización es necesaria antes de ampliar el historial y la búsqueda. |
| Markdown, código y diff | `elements/gui/formatted_text_element.rs:145-240` representa texto formateado con selección y enlaces. Las capturas de Warp muestran diff y code review; **ese producto no puede atribuirse al crate WarpUI sólo por la imagen**. | `rich_text.rs` presenta Markdown acotado sobre spans medidos; `git_review.rs` agrupa diff por archivo/hunk con números de línea y archivos sin seguimiento. Un worker lo carga fuera del hilo visual. | Falta selección/copia de texto estático y enlaces activables. La revisión completa requiere edición/comentarios sobre hunks y, en el futuro, un editor/PTY propios. |

`openide-ui/src/typography.rs` ofrece una escala semántica y ajuste por ancho
medido por el renderer, con límites de grafemas Unicode. La API está exportada
y el host egui instala la medición real en los dos árboles interactivos.

## Tres fases en orden de impacto

### 1. Coherencia visual y capas de interacción

- Completar `Overlay` con `Popover`, `Menu`, `Tooltip` y `Dialog`, bloqueo modal, Escape/clic exterior y foco restaurado. Migrar Command Center a esa capa.
- Crear componentes `Button`, `IconButton`, `Tab`, `Badge`, `Field`, `Card` con tokens de altura, padding, radios y estados definidos una vez. Mantener el icono Solar de **16 px lógicos** en todos los controles; el área clicable puede ser mayor.
- Medir tipografía con el mismo backend que rasteriza el texto y usar estilos semánticos sans/mono. Centrar la columna legible del timeline y composer con límite de ancho. Agregar truncamiento por ancho real.
- Criterio visual: captura a 100 % y 200 % DPI sin cortes de texto ni iconos borrosos; menús flotantes no mueven los docks; foco y hover conservan geometría.

### 2. Densidad de información y fluidez

- Agregar lista virtualizada para sesiones, archivos y eventos con altura variable y ancla de scroll; medir sólo filas visibles más un margen pequeño.
- Representar texto enriquecido de respuestas, código, enlaces y resultados de herramientas con segmentos de estilo, selección/copia y bloques expandibles. Implementar diff con modelo de archivo/hunk/línea y colores semánticos discretos.
- Añadir scrollbars y estados visuales para seleccionado, activo, deshabilitado y en progreso. Animar apertura de paneles, selección y entradas del timeline con movimiento reducido.
- Criterio funcional: 1.000 sesiones y una transcripción extensa conservan scroll y foco al llegar eventos; el coste de layout depende principalmente de filas visibles.

### 3. Renderer y superficies avanzadas

- Perfilar escenas reales antes de sustituir el host egui. Si aparecen límites, crear un backend wgpu propio para la misma interfaz `Frame`, con capas, scissor, atlas de glifos/imágenes y efectos compartidos.
- Agregar pane de revisión de código, terminal PTY y navegador embebido como superficies con ciclo de vida, foco y clipping explícitos; conectar cambios de archivos y herramientas a esas vistas.
- Criterio: paridad visual y de input entre escalas DPI, frame time estable bajo streaming, recovery de dispositivo y pruebas de screenshot para los estados principales.

## Procedencia y licencia

`references/warp/README.md:54-56` declara `warpui_core` y `warpui` bajo MIT y el resto del repositorio bajo AGPL v3. Ambos manifests, `references/warp/crates/warpui_core/Cargo.toml:7` y `references/warp/crates/warpui/Cargo.toml:7`, indican `license = "MIT"`; las licencias completas están en `references/warp/LICENSE-MIT` y `references/warp/LICENSE-AGPL`. Esta auditoría observa diseño y comportamiento del framework MIT. El producto OpenIDE mantiene implementación original; no se traslada código de las áreas AGPL. Si en el futuro se incorpora código MIT de esos dos crates, corresponderá conservar su aviso y atribución.
