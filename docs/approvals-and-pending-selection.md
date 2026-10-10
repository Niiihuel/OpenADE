# Aprobaciones y selección durante una respuesta

## Revisión del permiso

La operación conserva una tarjeta en el historial. Cuando el ejecutor solicita
permiso, la vista previa se convierte en la misma fila de actividad con estado
«Esperando autorización», antes de recibir `ToolStarted`. Un parche que elimina
un archivo muestra la operación propuesta, sin un editor de código vacío.

`openide_ui::ApprovalRequest` presenta una operación y su destino con la acción
«Revisar». La aplicación muestra también un aviso encima del composer, fuera del
scroll del historial. El aviso no abre ventanas ni roba el foco al escribir.
Desde ambas superficies se abre el mismo diálogo controlado por `ApprovalUiState`.

El diálogo distingue permitir una vez, denegar y permitir la misma operación en
la sesión. Esta última decisión conserva el alcance existente del harness:
herramienta, argumentos y contexto; no equivale a autorizar todas las llamadas de
una herramienta. Los detalles técnicos se expanden a petición y su vista está
limitada a 16 KiB. La decisión siempre corresponde a los argumentos completos.

Cerrar con el control `Esc` o con el teclado deja la solicitud pendiente. Enter
sobre el cierre tampoco concede ni deniega. El host valida conversación,
proyecto, subagente y solicitud antes de cada decisión. Cambiar de conversación
o resolver la solicitud invalida el diálogo; no se reutilizan decisiones de una
vista antigua. Los permisos de hijos se pueden revisar desde su sesión o desde
la sesión padre relacionada.

## Elección para el siguiente mensaje

El selector de modelo, esfuerzo y permisos sigue disponible durante una respuesta.
La elección se presenta como «Para el próximo mensaje», con «Deshacer». Volver a
elegir reemplaza la selección pendiente. El modelo y los permisos efectivos del
turno actual permanecen estables, incluida cualquier aprobación que esté esperando.
Las indicaciones enviadas mientras el turno continúa son steering del turno actual
y siguen usando su configuración vigente.

La selección pendiente pertenece a la conversación y al proyecto donde se creó.
Sólo se aplica cuando termina el worker del turno; un evento terminal del historial
no basta mientras siga activo su proceso de cierre. Antes de aplicar se vuelven a
validar el catálogo, los permisos guardados y el consentimiento de acceso completo.
Si la elección ya no es válida, se conserva un aviso accionable en el composer.

La transición de modelo conserva los mensajes y las parejas de llamadas/resultados
de herramientas. Los datos opacos de una ruta anterior no se reutilizan en otra:
la copia materializada del historial y su replay aplican la misma regla, sin
modificar los eventos originales. Los checkpoints de formatos textuales v1
conocidos se trasladan mediante un candidato y un marcador nuevos, con el mismo
resumen y cobertura; no se solicita una compactación remota ni se informa una
estimación de tokens del modelo anterior como una medición del nuevo.

## Historial y cierre

El historial de salida consulta páginas del registro durable de comandos nativos.
Abrirlo, actualizarlo o cerrarlo no inicia, reanuda ni detiene comandos. Los nombres
visibles usan el comando; las identidades técnicas siguen disponibles en Detalles.
El estado vacío es compacto y la paginación sólo aparece cuando hay otra página.

`DismissButton` reutiliza `Kbd` para el cierre visible `Esc`, centra el control
sobre su área de interacción y permite separar su identidad de la acción emitida.
`Modal`, `AlertDialog` y el buscador lo comparten. El cierre restaura el foco; el diálogo de
aprobación mantiene sus decisiones separadas del cierre.

La comparación se hizo con `references/zed/crates/ui/src/components/modal.rs`,
`references/zed/crates/command_palette/src/command_palette.rs` y
`references/zed/crates/tasks_ui/src/modal.rs`, además de la
[documentación de permisos de Zed](https://zed.dev/docs/ai/tool-permissions).
Se adopta la separación entre resumen de operación, decisión y detalles. El
historial de OpenIDE sigue siendo un visor de su journal de salida; no se afirma
paridad con el historial de tareas o la terminal de Zed. Tampoco se cambia el
alcance de permisos de sesión a las reglas permanentes de Zed.

## Validación

Las pruebas cubren estados de permiso y replay, cierre sin decisiones, solicitudes
obsoletas o ajenas, revisión padre/hija, selector pendiente y validación antes de
aplicar. Los escenarios visuales incluyen borrado por parche, comando y operación
MCP a 350 y 900 píxeles, con detalles abiertos y cerrados. La suite de exportación
`export_approval_visual_review` escribe PNG y SVG en `/tmp/openade-approval-review`.
