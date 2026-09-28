# Calidad visual del framework propio

La documentación de WarpUI y sus módulos de elementos, componentes y
renderizado sirvieron como referencia conceptual. El código de esta
implementación es original. El README de Warp declara `warpui` y `warpui_core`
MIT; el resto del repositorio es AGPL v3. No debemos incorporar código de las
partes AGPL en OpenIDE.

## Decisiones aplicadas

- El árbol de elementos describe cada frame; `UiTree` conserva por ID el foco,
  scroll y ahora las transiciones de botones. Reconstruir el árbol no reinicia
  una animación ni altera el área clicable.
- El fondo de hover y pulsación interpola durante 120 ms con salida cúbica.
  Una nueva interacción puede cambiar de destino a mitad de la transición sin
  saltos. El color ausente se trata como transparencia del mismo tono, para
  evitar un borde gris al entrar o salir.
- `Element::transition_ms(0)` y `UiTree::set_reduced_motion(true)` ofrecen
  respuesta inmediata. El host solicita nuevos frames sólo mientras
  `UiTree::is_animating()` sea verdadero.
- Los iconos de controles usan la medida única de 16 px lógicos; el backend
  escala al DPI y mantiene la misma caja óptica en cada dock.
- El foco de teclado usa un trazo interior con el mismo radio del botón;
  conserva su forma en los controles redondeados y no cambia la geometría.
- El árbol mide texto con el mismo motor de fuentes del host: párrafos,
  botones, inputs y spans de Markdown usan anchos reales y cortes de grafema.
- `Element::overlay` difiere su pintura y su hit testing al nivel superior;
  `Element::shadow` añade elevación sin ampliar los límites de interacción.
- El scroll vertical muestra una pista y thumb proporcionales y permite
  arrastrar o hacer clic en la pista sin tapar el contenido.

## Criterios para el siguiente nivel de UI

1. **Layout:** espacios y alturas de controles semánticos, con anchura de
   lectura acotada para el composer y contenido largo. Mantener el sidebar de
   sesiones separado de la superficie compartida por centro y dock derecho,
   mínimos razonables y un divisor interior de 1 px lógico.
2. **Capas:** migrar menús, tooltips y Command Center a la capa flotante del
   framework. Un modal debe atenuar el fondo, restringir el foco y cerrar con
   Escape o clic exterior.
3. **Tipografía:** completar selección/copia de texto estático, pesos y
   cursivas reales, enlaces accionables y caché de layout para sesiones largas.
4. **Estados:** cada control necesita estado normal, hover, pulsado, enfocado,
   seleccionado y deshabilitado. El foco de teclado tiene un contorno visible
   distinto del hover; las transiciones afectan pintura, no geometría.
5. **Renderizado:** mantener radios, bordes, clips y sombras coherentes al
   sumar popovers, diffs y contenidos incrustados.
6. **Movimiento:** duración breve para microinteracciones, ninguna animación
   que retrase una acción. Respetar movimiento reducido en todo componente
   futuro, incluidas aperturas de paneles y menús.

Referencia local: `references/warp/crates/warpui_core/README.md`,
`references/warp/crates/warpui_core/src/elements/gui/`,
`references/warp/crates/warpui_core/src/ui_components/`,
`references/warp/crates/warpui/src/rendering/`.
