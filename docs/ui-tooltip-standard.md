# Tooltips de la ventana nativa

El componente compartido es `openide_ui::Tooltip` (`crates/openide-ui/src/components.rs`). Envuelve el control existente con su ID de acción: muestra la misma cápsula flotante y, si corresponde, el atajo con `Tooltip::shortcut`. No agrega área de clic ni altera la medida del control. `UiTree` lo presenta tras hover o inmediatamente con foco de teclado; lo coloca dentro del viewport y fuera del clip de los scrolls. Para `Button` e `IconButton`, también se puede usar `.tooltip(...)` y `.tooltip_shortcut(...)`.

## Inventario de uso

| Superficie | Acciones con tooltip compartido |
| --- | --- |
| Sidebar y navegación | Plegar/reabrir, ajustes, orden, crear grupo, archivadas y menús de tarea/grupo. |
| Composer | Proyecto, modo, permisos, adjuntos, modelo/esfuerzo, enviar/detener y chips de archivos. |
| Búsqueda global | Cierre con `Esc` en el control de la cabecera. |
| Dock derecho | Abrir pestaña, paginar tabs, refrescar archivos y cambios de Git, overview, ocultar panel, cerrar tabs y preview. |
| Marketplace | Actualizar catálogo, cerrar formulario, desinstalar skill, quitar origen y activar/desactivar skill. |
| Automatizaciones | Editar, ver historial, ejecutar ahora, eliminar y activar/pausar tarea; elegir plantilla. |

Los botones con texto visible y las filas de menú conservan su etiqueta. Sus atajos se dibujan con `Kbd`; los tooltips se reservan para ayuda contextual, acciones sólo con icono o texto truncado. Cuando un menú está abierto, su disparador puede omitir el tooltip para que la ayuda no cubra el popover; el mismo disparador lo muestra mientras el menú está cerrado.

Al agregar una acción sólo con icono, usar `Tooltip::new(anchor, id, título, palette)` o las opciones de tooltip del botón compartido. El `id` del tooltip debe coincidir con el ID del control para conservar hover, foco y activación.
