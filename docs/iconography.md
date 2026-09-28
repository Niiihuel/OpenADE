# Iconografía

## Convención de tamaño

Todos los iconos de la interfaz usan **16 px lógicos**. El token único es
`openide_ui::ICON_SIZE`; `Element::icon`, los botones con icono y el renderer lo
aplican también si un llamador intenta solicitar otro tamaño. El botón puede
tener una superficie interactiva mayor (mínimo actual de 24 × 24 px en los
controles compactos), pero el arte permanece centrado en una caja de 16 × 16.
No se asignan tamaños por icono ni por pantalla.

Al rasterizar, 16 px lógicos equivalen a 16, 20, 24 o 32 píxeles físicos en
pantallas con escalas 100 %, 125 %, 150 % o 200 %. La posición y el tamaño
final se alinean a la cuadrícula física; la textura usa el tamaño físico final
y filtro `NEAREST` para evitar suavizado adicional. Todos los recursos Solar
tienen `viewBox="0 0 24 24"`; el renderer conserva ese encuadre y nunca estira
un icono por separado. El tamaño óptico de cada forma puede variar levemente
dentro de la caja por el dibujo original.

La aplicación usa los SVG **Linear** de Solar Icons incluidos en
`apps/agents-window/assets/solar/`. Los 24 archivos se extrajeron sin modificar
de `@solar-icons/static` 2.3.2. El renderer genera una máscara blanca a la
escala física y conserva la textura en una caché por icono, tamaño y contexto
de egui. El color se aplica al dibujar según el estado del control; no se
rasteriza de nuevo para hover, selección o cambios de tema. La iconografía del
tema nativo es monocroma y neutral. Los SVG originales y su atribución figuran
en `THIRD_PARTY_NOTICES.md`.

Como referencia visual y arquitectónica, revisamos
`references/warp/crates/warpui_core/src/elements/gui/icon.rs` y
`references/warp/crates/warpui_core/src/ui_components/button.rs`. El elemento
monocromo de Warp centra el recurso dentro de sus límites, lo ajusta con
`Contain`, almacena la imagen por tamaño y aplica el color al dibujar. Los
botones heredan el color de texto del estado activo para teñir el icono.
Nuestro renderer sigue esos criterios con código propio; los botones tienen
estados hover y pressed con fondos del tema.

La API de `openide-ui` usa nombres semánticos (`Icon::Folder`, `Icon::Settings`,
etc.), sin acoplar los componentes a rutas SVG. El host escoge el arte
concreto en `apps/agents-window/src/render.rs`.
