# Cursor Settings: relevamiento e integración en openADE

Referencia local: **Cursor 3.17.21**, distro `d5c0e77a0214208f36b56d42e8e787de88d02ea4`, analizada el 15 de octubre de 2026. Se inspeccionó el paquete Electron instalado en `/nix/store/ij7hsdlff78ksdqx68gl2i4rnff57x8x-cursor-3.17.21/lib/cursor/resources/app` y especialmente `out/vs/workbench/workbench.glass.main.js`.

SHA-256 del bundle Glass: `b6f06229dc965619ab318e3bbc9f102865101a3bc0f346630532e2558f6fc398`.

## Evidencia y alcance

El [inventario estructurado](cursor-3.17.21.json) contiene los 21 destinos de navegación observados, 149 entradas del catálogo de Settings, 464 campos literales de presentación en regiones acotadas y 153 defaults tipados. Los conteos incluyen grupos, enlaces y metadatos: **no son 464 preferencias independientes**. Se revisaron los módulos de la navegación, no todas las preferencias heredadas de VS Code.

Se usó REA 6.4.0, sin ejecutar el bundle. El análisis global agotó el heap de su worker y conservó evidencia parcial. No se considera un análisis semántico completo de Cursor:

- `ev_9895e1d9d86b5552fca20ca1c51549d366c6380e964b168be66df6372a7593e2`: inventario parcial del directorio workbench; 27 archivos relevantes, con cobertura incompleta.
- `ev_1a2651741f58b2d3ae69f011d40679d9fcfa106107e5f1c15a6923620502595c`: análisis focalizado de tres rangos exactos seleccionados por el analista, correspondientes a Appearance, Typography y Density. Se parsearon los tres rangos; las referencias minificadas a servicios externos a esos rangos permanecen desconocidas.
- `ev_882918500ffb0149c92a3b98cd7bca72444e08b472bb0fa9609e953ccd96ba69`: traza parcial de `uiFontSizePx`, con su propiedad observada y sus fronteras dinámicas sin resolver.

[rea-projection.json](rea-projection.json) conserva offsets en bytes y hashes de cada rango, para reconstruirlos desde el bundle original. El bundle portable completo de REA se exportó a `target/cursor-settings-rea/evidence.json` (artefacto local ignorado por Git). Los snippets propietarios analizados permanecen fuera del código de openADE.

Los labels, rangos, defaults y llamadas a setters que se describen aquí son **observaciones estáticas**. La adaptación de esos comportamientos a Rust es una decisión de implementación propia. No se probaron opciones en el runtime de Cursor; no se verificaron experimentos activos, políticas de una cuenta, facturación, endpoints remotos ni enforcement de permisos.

## Apariencia observada

- Theme: selección de tema y modo del sistema. El soporte avanzado de temas agrega selección por esquema, sincronización con IDE, alto contraste e importación, bajo un flag de producto.
- Tool Call Density: tres posiciones visibles, `compact-all-grouped`, `compact-ungrouped` y `detailed`. El default Glass es agrupado; se migran valores anteriores como `compact-shells` y `compact-grouped`.
- Code Block Word Wrap: `cursor.composer.codeBlockWordWrap`, default `false`.
- Themed Diff Backgrounds: `cursor.cmdk.useThemedDiffBackground2`, default `true`.
- Hue e Intensity: almacenamiento separado `glass.theme.customTintHue` y `glass.theme.customTintIntensity`, defaults 210 y 0; el selector de tono recorre 0–360 y el de intensidad 0–100 %. Los colores semánticos y superficies se derivan del tema; la persistencia del tinte tiene debounce.
- Reduce Transparency: `cursor.general.reduceTransparency`, default `false`. Una preferencia del sistema puede forzar la opción activada y deshabilitar el switch de Cursor.
- UI Font Size: enteros de 7 a 38, default 13. Code Font Size: enteros de 10 a 22, default 12.
- UI Font Family y Code Font Family: overrides opcionales de fuentes instaladas; se guardan junto a tamaños bajo `glass.display.settings`. Cursor permite además otras familias para código.
- Font Smoothing: `cursor.general.fontSmoothingAntialiased_v2`, default `true`, sólo se presenta en macOS.
- Follow System High Contrast: preferencia independiente del tema claro/oscuro, default local `true`; puede quedar administrada por políticas del IDE.
- Reduce Motion: `workbench.reduceMotion`, valores `on`, `off`, `auto`; default System.
- Hide Email Address: `cursor.general.emailPrivacyEnabled`, default `false`; es una máscara de presentación.
- Confetti Cannon: control condicionado por flag; no es una opción universal de esta build.

## Adaptación nativa

`SettingsPreferences.appearance` conserva valores validados dentro del JSON de preferencias existente. Cada campo se recupera independientemente si sus vecinos son inválidos. El switch anterior de movimiento se migra sin perder una elección explícita.

Las opciones de conversación controlan el renderer real y respetan las expansiones manuales. Agrupada conserva las secuencias de actividad; Equilibrada muestra tarjetas individuales cerradas; Detallada las abre. Cambiar densidad también invalida la proyección de diffs para cargar el contenido que pasa a estar visible. Las aprobaciones, los errores y el razonamiento mantienen su presentación propia.

Los sliders de tinte tienen preview en memoria y guardan al confirmar. El renderer modifica superficies nativas sin recolorear imágenes o páginas del navegador. Reducir transparencia sustituye el material Glass por una superficie opaca, conservando la opacidad de sus transiciones. Los temas de alto contraste son variantes propias de openADE.

La interfaz usa zoom de egui para que controles, layout y punteros conserven la misma escala. El tamaño de código se compensa por ese zoom y llega a CodeEditor, CodeDiff, CodeBlock y Markdown. Las nuevas propiedades de limn gobiernan medición, filas, selección y pintura. Las fuentes instaladas conservan los fallbacks Unicode del framework; una fuente guardada ausente en otro equipo utiliza el fallback disponible.

El refinamiento de los controles reemplaza las tarjetas de temas por un único
Select con integrados y personalizados. Los tamaños son NumberInput editables;
las familias tienen búsqueda en el menú y una muestra CodeDiff nativa permite
comprobar fuente, sintaxis y fondos. El relevamiento focal de [tipografía y
colores](typography-colors.md) explica las opciones observadas, las diferencias de
descubrimiento de fuentes y por qué el tinte no cambia las superficies con
intensidad cero. En este host, fontdb detectó 269 familias, 10 monoespaciadas;
ese inventario es propio del equipo y no un catálogo distribuido con la aplicación.

Las mejoras iniciales del framework están en [limn PR #43](https://github.com/Niiihuel/limn/pull/43). El [PR #45 de limn](https://github.com/Niiihuel/limn/pull/45) agrega el header opcional de Select para buscar fuentes y botones separados alrededor de un valor centrado en NumberInput; esta integración fija su revisión `ecdbeceab1aa21d656571d7a70a484290e4a5a3d`. Se agregaron identidades estables para los viewports de código: dos mensajes idénticos mantienen posiciones de scroll independientes. Los viewports anónimos se podan al reemplazar el árbol para acotar su estado.

Límites explícitos de esta adaptación:

- El selector de código ofrece fuentes monoespaciadas para conservar la geometría de columnas del editor nativo.
- El tinte modifica los temas integrados. En temas personalizados, los sliders se deshabilitan y remiten a Editar tema para conservar los colores elegidos.
- El modo System de movimiento consulta el portal de escritorio en Linux; las otras plataformas conservan el fallback de `OPENIDE_REDUCED_MOTION`. El tema System sigue claro/oscuro del host; los temas de alto contraste se eligen manualmente.
- El switch de suavizado controla la colocación subpíxel real de egui, preservando hinting, pesos y antialiasing. Su efecto difiere del antialiasing nativo de macOS. Véase [TextOptions de egui](https://docs.rs/egui/0.36.2/egui/struct.TextOptions.html).
- El editor/importador de temas se incorpora como capacidad local de openADE. El efecto de confetti observado en Cursor no se incorpora.

## Segunda etapa de Apariencia

La biblioteca permite crear, editar, duplicar, aplicar, exportar y eliminar hasta 30 temas. Importa YAML de temas y JSON estricto de colores de VS Code, valida hasta 1 MiB sin resolver archivos o recursos remotos y conserva 43 tokens para intercambio. El editor usa ColorPicker de limn y expone 18 grupos con efecto independiente en openADE; los otros 25 tokens se conservan para exportación, incluidos los colores de terminal. Los aliases de la interfaz nativa se editan juntos. El alto contraste claro es una adaptación propia, con fondos claros y colores de texto comprobados contra WCAG AA; Cursor distribuye un único bucket de alto contraste oscuro en el código observado.

Los borradores no cambian la biblioteca hasta guardar y aplicar. Cambiar de base conserva los colores personalizados; cancelar o fallar una escritura conserva las preferencias anteriores. Los selectores de archivos trabajan fuera del hilo de UI y cancelar no abre un segundo backend. Una selección guardada sin tema válido cae al tema oscuro. Más detalle de la referencia y compatibilidad: [temas personalizados](custom-themes.md).

El [contrato de implementación](appearance-completion.md) registra los controles, límites y verificaciones. La rasterización se verifica con el contexto real de egui; Windows/macOS y sus selectores nativos requieren aceptación en esas plataformas.

## Resto de módulos

El [relevamiento focal de MCP y Marketplace](mcp-marketplace.md) registra una segunda
proyección de cinco rangos exactos, su evidencia REA y el contrato de adaptación
de lista, detalle, herramientas y procedencia.


Los detalles y offsets verificables de cada opción están en el inventario JSON. La [generación de imágenes](image-generation.md) tiene un relevamiento focal adicional que distingue la herramienta, la ruta HTTP local y el servicio remoto, sin inferir precios o condiciones de suscripción.

- **General:** cuenta, importación de preferencias, layout del IDE, restauración al iniciar, avisos, bandeja, sonido de finalización, privacidad y firma de extensiones. Parte de la privacidad puede estar administrada por equipo.
- **Profile:** foto, nombre, handle, enlaces y perfil público, con validación y guardar/cancelar. Requiere API de perfil y disponibilidad de cuenta.
- **Agents / Chat:** tamaño de conversación, envío, cola/steer/interrupt, resumen de uso, recomendaciones, transiciones, voz, web, autenticación MCP, modos de ejecución, allowlists, terminal, edición, formato y control remoto. La UI no sustituye el enforcement del harness.
- **Cloud Agents:** dashboard, conexión Slack y readiness según repositorio y modo de privacidad. Las cargas y errores tienen estados propios.
- **Models:** buscar/agregar/quitar/habilitar modelos, modelo Explore, proveedores y credenciales, endpoints OpenAI/Azure/Bedrock, gateway bajo flag. Catálogo y acceso son hechos distintos.
- **Git & PRs:** proveedor, revisión integrada/browser, revisión automática, Quick/Deep, submódulos, archivos no seguidos, atribución y prefijo de rama. Las preferencias del equipo pueden prevalecer.
- **Worktrees:** máximo 25 por defecto, rango 1–500; tamaño agregado 50 GB por defecto, rango 0–1000, donde 0 deshabilita el límite. El intervalo de limpieza registrado es de 6 horas, sin control visible observado. No se ejercitó eliminación.
- **Self-driving:** agentes sobre PRs propios, autoarranque, sugerir/aplicar y notificaciones Slack. Requiere servicio y flag de cuenta.
- **Customize:** destino unificado y migración de las páginas primitivas. La etapa de migración determina qué pestañas aparecen.
- **Plugins:** instalados/locales/marketplace, contenidos y confirmación de eliminación. La instalación y ejecución tienen contratos separados de la UI.
- **Rules:** ámbitos usuario/proyecto/workspace/equipo, skills/subagentes/comandos, importación de configuraciones y GitHub/GitLab.
- **MCP:** servidores por ámbito y procedencia, herramientas/estado/habilitación/auth, búsqueda/orden/agregar/editar/quitar. La configuración de nube o equipo no equivale al archivo local `.cursor/mcp.json`.
- **Hooks:** configuración, errores y registro de ejecución; `hooks.json` por ámbito. No se ejecutaron hooks durante el relevamiento.
- **Browser & Network:** automatización, protección, enlaces locales/web, compatibilidad HTTP y diagnósticos DNS/HTTP2/TLS/API/stream/auth/marketplace. No se ejecutaron pruebas de red.
- **Tab:** sugerencias, aceptación parcial, comentarios/whitespace/imports y patrones ignorados. Requiere backend de completado y editor.
- **Code Intelligence / Indexing:** LSP, concurrencia local/remota (default 3 cada una), estado/borrado de índice, nuevas carpetas, Grep, ignores, symlinks y repositorios de equipo. No se modificó ningún índice.
- **Beta:** canal de actualización, capacidades experimentales del lateral y runtime dedicado/bridge. Algunas opciones requieren recarga y elegibilidad.
- **Developer:** trazas, RPC recording, logs y controles de desarrollo. No se activó grabación.
- **Plan & Usage:** consumo personal/empresa, créditos, uso adicional y límite mensual fijo/ilimitado/desactivado. Depende del backend de facturación y roles administrativos.
- **Docs y VS Code Settings:** destinos externos/delegados; no son formularios locales equivalentes a Apariencia.

Para incorporar los demás módulos, el inventario permite reutilizar taxonomía, dependencias y validaciones observadas. Cada capacidad debe conectarse al servicio real de openADE antes de ofrecer un control editable.

## Estado y validación

La identidad del artefacto, la navegación y los metadatos locales de Settings quedaron relevados. Apariencia quedó integrada en el renderer nativo dentro de los límites indicados; los demás módulos tienen inventario y revisión, sin controles de backend simulados. Los efectos de políticas remotas y la visibilidad efectiva de flags en una cuenta de Cursor permanecen sin verificar.

Se verificaron guardado/reapertura, recuperación de valores inválidos, preview sin escrituras, densidad y overrides manuales, tamaños independientes de interfaz/código, aislamiento del scroll de mensajes idénticos y límites de controles a 360/900 px. Las exportaciones SVG/PNG están en `target/appearance-visual-qa`; comprueban presentación y geometría del árbol, sin sustituir una aceptación visual en el renderer GPU del desktop.

La compilación de producción (`cargo check -p agents-window --bin agents-window`) y las 31 pruebas de `openide-ui` pasaron. La actualización de limn requirió adaptar fixtures antiguos de gráficos, selección y metadatos al contrato vigente, conservando sus comprobaciones de datos, geometría, tooltips y acciones.

La primera etapa terminó con **2.354 pruebas aprobadas, 0 fallidas y 178 ignoradas preexistentes** en la suite completa de `agents-window`. El registro local está en `target/appearance-test-validation.log`.

La segunda etapa terminó con **2.385 pruebas aprobadas, 0 fallidas y las mismas 178 ignoradas preexistentes**, más las **31 pruebas de `openide-ui`**. Después del ajuste final de navegación del editor se verificaron de nuevo sus **13 pruebas de integración** y la compilación de producción, ambas correctas. Los registros están en `target/appearance-completion-test-validation.log`, `target/appearance-completion-editor-tests.log` y `target/appearance-completion-check.log`.

El editor abre al comienzo de la página, incluso con una biblioteca larga. Se verificaron sus controles, foco y geometría a 360/900 px; los SVG y PNG están en `target/appearance-completion-visual-qa`. Los contextos reales de egui prueban la transición de rasterización, sin sustituir aceptación del desktop GPU ni las pruebas nativas de Windows/macOS.


## Refinamiento de controles y retirada del efecto

Tema pasó a un único selector; NumberInput muestra decremento, valor centrado e
incremento; los menús de fuentes tienen búsqueda y la muestra CodeDiff espera el
resaltado asíncrono. El dispatcher comparte una sola regla de ámbito para abrir y
conservar los menús, incluida la configuración de imágenes. La explicación de
comandos usa una fila de la tarjeta. El efecto de confetti, su preferencia y su
herramienta se retiraron; el JSON anterior se recupera ignorando esa clave.

La suite final de `agents-window` pasó con **2.395 pruebas aprobadas, 0 fallidas y
178 ignoradas preexistentes**, en `target/appearance-refinement-test-validation.log`.
Los renders comprobados están en `target/appearance-refinement-visual-qa` y
`target/settings-choice-final`; verifican el árbol nativo, sin equivaler a una
prueba GPU de Windows/macOS. Limn pasó 1.210 pruebas de biblioteca, sus doctests,
Clippy y rustdoc, y el CI del PR #45 estaba aprobado al verificarlo.

La compilación optimizada de escritorio pasó, junto con las 31 pruebas de `openide-ui`; registros en `target/appearance-refinement-desktop-build.log` y `target/appearance-refinement-ui-test-validation.log`.
