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
`apps/agents-window/assets/solar/`. Los archivos se extrajeron sin modificar
de `@solar-icons/static` 2.3.2. El renderer genera una máscara blanca a la
escala física y conserva la textura en una caché por icono, tamaño y contexto
de egui. El color se aplica al dibujar según el estado del control; no se
rasteriza de nuevo para hover, selección o cambios de tema. La iconografía del
tema nativo es monocroma y neutral; los avisos semánticos de `Alert` tiñen
su icono con el color del estado y lo acompañan de un título explícito. Los
SVG originales y su atribución figuran
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

`Replace` y `ReplaceAll` usan SVG originales de `assets/openide/`: letras de origen y destino unidas por una flecha; las líneas adicionales distinguen el reemplazo de todas las coincidencias.

Los avisos usan `InfoCircle`, `CheckCircle`, `WarningTriangle` y `ErrorCircle`.
Este último reutiliza el arte Solar `close-circle` como símbolo de error; el
control para cerrar un aviso conserva el botón Ghost con el icono `CloseBold`.

## Proveedores de modelos

`provider_icons::icon_for_provider` es el único resolutor de identidad para el
composer, los modelos, las conexiones y las cuentas. Codex usa `Icon::Codex`,
distinto de `Icon::OpenAi`; OpenRouter, OpenCode, NVIDIA, DeepSeek, MiniMax y
GLM/Z.ai, Anthropic, Groq, Mistral y xAI tienen isotipos propios. Un proveedor desconocido conserva `Grid`, sin
asignarle por similitud la marca de otro servicio. Los alias exactos `zai`,
`z-ai`, `z.ai` y `glm` comparten Z.ai.

Los recursos de Codex, OpenAI, OpenRouter, OpenCode, NVIDIA, DeepSeek, Anthropic,
Groq, Mistral y xAI provienen
de [SVGL](https://svgl.app/), fijado al commit
`f9b726cee1d1f3c697a18c880185e89d9a55bb70`. Se consultó su
[API oficial](https://svgl.app/docs/api) durante la integración, pero la
aplicación **no llama a esa API**: todos los SVG se vinculan al binario con
`include_str!`. MiniMax y Z.ai no estaban en el catálogo consultado y usan
`@lobehub/icons-static-svg` **1.95.1**, bajo MIT y con atribución separada.
Se comprobó el glifo Z.ai contra el SVG de su sitio oficial, sin redistribuir
ese recurso de licencia no declarada.

`assets/providers/SOURCE.json` registra las URL originales, revisión o versión,
hashes SHA-256 y adaptaciones. Los originales se conservan junto a las máscaras
nativas derivadas. Estas sólo cambian el color por `currentColor`, eliminan CSS
innecesario y fijan dimensiones SVG. OpenCode omite su placa oscura de fondo y
encuadra el isotipo, manteniendo sus trazados y el detalle interior con opacidad.
Groq también omite su placa de fondo; Groq y xAI encuadran el trazado original
sin márgenes vacíos. OpenAI encuadra los extremos reales de sus curvas y conserva
un margen óptico de 1.5 % por lado. Mistral representa su sombra original con opacidad.
El renderer y el exportador comparten `provider_icons::svg_for_icon`.

Cada marca ocupa la misma caja lógica de 16 × 16, centrada con **Contain**:
NVIDIA y OpenCode conservan su proporción, sin estirar el arte. La máscara
blanca se rasteriza una vez por tamaño físico y se tiñe al pintar; tema claro,
oscuro, hover y opacidad no requieren nuevas descargas ni nuevas texturas.
Las marcas comerciales siguen perteneciendo a sus respectivos titulares;
la licencia del catálogo no representa aprobación ni afiliación de éstos.

La prueba ignorada `export_provider_icon_visual_review` genera una matriz con
los doce proveedores, opacidad normal/reducida y temas claro/oscuro a 1× y 2×
en `target/provider-icons-visual-qa/`. Las pruebas normales comprueban hashes,
recursos sin dependencias externas, rasterización nativa, proporción y límites
de exportación.

Las modalidades de los modelos usan `Icon::Text`, `Image`, `Audio`, `Video`
y `File`, con los recursos Solar `text-square`, `gallery`, `soundwave`,
`videocamera` y `file-text`. En las fichas informativas se acompañan de una
etiqueta corta para conservar el significado sin ayuda anidada.

## Marcas elegibles para integraciones

`brand_icons` ofrece el catálogo completo de SVGL para presentaciones como los
servidores MCP. No reutiliza el mapa de proveedores ni deduce identidades desde
IDs internos. La elección manual se conserva en la configuración cosmética del
host; no altera comandos, permisos, herramientas ni las huellas de recuperación.

El snapshot oficial contiene 670 registros y 831 rutas SVG. Dos registros de
PyCharm comparten exactamente arte y sitio: se presentan como una sola entrada
con ambos nombres de búsqueda, por lo que hay 669 artes seleccionables. Los IDs
se derivan de las rutas light/dark mediante SHA-256, no del orden numérico ni del
título. Títulos iguales con arte distinto conservan entradas independientes.
La búsqueda prioriza nombres o aliases exactos, después prefijos, coincidencias
parciales de nombre y, finalmente, categoría o sitio; los empates conservan el
orden del catálogo.
El catálogo, la licencia MIT y los hashes originales/derivados están en
`assets/brands/svgl/`. La revisión fijada es
`f9b726cee1d1f3c697a18c880185e89d9a55bb70`.

La API de [SVGL](https://svgl.app/docs/api) documenta las variantes de tema y
recomienda cachear respuestas. Aquí la caché es el snapshot distribuido: la
aplicación no descarga iconos, no consulta dominios de servidores o paquetes y
no transmite términos de búsqueda. `scripts/vendor-brand-icons.py` reproduce
los recursos desde el archivo oficial fijado por revisión y hash; también puede
usar `--archive` para ejecutarse sin red y `--check` para verificar el resultado.
La actualización explícita exige revisar ambos pins y el nuevo catálogo.

El script elimina scripts, handlers, metadatos y declaraciones DTD sin resolver
recursos externos. Rechaza entidades, referencias externas, CSS activo y archivos
que excedan los límites. Conserva colores, geometría, gradientes y referencias
internas; resuelve `currentColor` según el tema. Cinco SVG originales contienen
imágenes PNG o texto dependiente de fuentes: se rasterizan durante el vendoring
a PNG de 64 × 64 mediante ImageMagick/librsvg para que el renderer nativo no
muestre una marca incompleta. No se instala software ni se rasterizan esos SVG
en la aplicación. La reproducción de esos cinco PNG requiere la versión indicada
en `SOURCE.json`; fuentes de texto del sistema pueden afectar ese derivado y
`--check` detecta diferencias.

`BrandEntry::element(dark)` conserva colores con tinta blanca en la caja habitual
de 16 × 16 y ajuste **Contain**. `icon(dark)` sólo está disponible para arte
vectorial: introducirlo en un botón que tiñe sus iconos puede cambiar los colores,
por lo que la interfaz de selección usa `element`. Los SVG son cadenas estáticas
incluidas en el binario; no hay un interner de SVG de duración ilimitada. Los cinco
PNG comparten una caché pequeña de payloads y las texturas las conserva limn.

La detección automática acepta un único arte asociado al host exacto del endpoint.
Para un proceso local exige dos indicios: un sitio oficial de metadatos que
coincida exactamente con una única entrada y un nombre de paquete o ejecutable
que corrobore esa misma marca. Un nombre de servidor elegido por el usuario no
basta para corroborarla. Sitios compartidos, nombres ambiguos, evidencia
contradictoria o un endpoint distinto conservan el símbolo genérico. Las demás
coincidencias se muestran como candidatos con su motivo y requieren elección
explícita. Un ejecutable de lanzamiento como Python o npx no representa la marca
de la integración. No se usan coincidencias por subcadena o sufijo de dominio.
Automático representa indicios locales, no una verificación de identidad o afiliación.

## Archivos, carpetas y lenguajes

Las identidades de recursos usan **Material Icon Theme 5.38.1**, con 165 SVG
vinculados al binario y licencia MIT. La procedencia, el commit y los hashes
están en `crates/openide-ui/assets/material-icon-theme/SOURCE.json`.
La actualización reproducible se realiza con `scripts/vendor-file-icons.py`.
[Paquete publicado](https://www.npmjs.com/package/material-icon-theme/v/5.38.1).

`openide_ui::file_icon(path)`, `folder_icon(path, expanded)` y
`language_icon(alias)` son los únicos resolutores de identidad. Priorizan
nombres especiales como `Cargo.toml` y `package.json`, luego extensiones; los
recursos desconocidos conservan un icono genérico. Carpetas abiertas y cerradas
comparten la misma familia. No leen archivos ni descargan recursos al pintar.

Files, pestañas de archivos y snapshots, breadcrumbs del editor/diff, archivos
del chat, resultados de búsqueda y adjuntos comparten esos resolutores. Los
iconos de acciones (abrir archivos, copiar, buscar) siguen siendo Solar.
El renderer conserva los colores originales del paquete y aplica sólo la
opacidad del estado; la caché está acotada a 512 texturas. Un hover no vuelve
a rasterizar el recurso.
