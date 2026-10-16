# Temas personalizados: evidencia de Cursor y adaptación nativa

Referencia: Cursor 3.17.21, bundle `out/vs/workbench/workbench.glass.main.js` del paquete identificado en [README.md](README.md), SHA-256 `b6f06229dc965619ab318e3bbc9f102865101a3bc0f346630532e2558f6fc398`. Todos los offsets siguientes son **bytes UTF-8**, con origen cero e intervalos de extremo final excluido.

Este documento reúne inspección estática focalizada del artefacto distribuido. No se ejecutó Cursor ni se comprobó la disponibilidad de flags para una cuenta. Los análisis REA previos y sus límites se conservan en el README y en [rea-projection.json](rea-projection.json). Los rangos de temas que se identifican aquí complementan esa evidencia; no se presentan como un nuevo análisis semántico completo de REA. El código propietario permanece fuera del repositorio de openADE.

## Modelo de colores y defaults observados

`WJ` declara 43 tokens core en `18514508`:

- Superficies y semántica: `sidebar`, `chrome`, `editor`, `base`, `accent`, `actionLabel`, `brand`, `focus`, `success`, `warn`, `danger`, `added`, `modified`, `removed`, `untracked`.
- Terminal: `terminalAnsiBlack`, `terminalAnsiRed`, `terminalAnsiYellow`, `terminalAnsiGreen`, `terminalAnsiCyan`, `terminalAnsiBlue`, `terminalAnsiMagenta`, `terminalAnsiWhite` y sus ocho variantes `terminalAnsiBright…`.
- Paleta auxiliar: `red`, `orange`, `yellow`, `green`, `cyan`, `blue`, `magenta`, `purple`.
- Diffs: `diffAddedLineBackground`, `diffAddedTextBackground`, `diffRemovedLineBackground`, `diffRemovedTextBackground`.

El paquete incluye tres objetos de defaults, expresados principalmente en OKLCH: `Uli` oscuro (`18540651`), `jli` de alto contraste oscuro (`18542508`) y `$li` claro (`18544379`). `Pge`, en `18548173`, los reúne como `dark`, `highContrast` y `light`. La función `Ywl`, en el rango de utilidades indicado abajo, selecciona el objeto de alto contraste siempre que `highContrast` sea verdadero: esta ruta no contiene un cuarto objeto independiente para alto contraste claro. Después se conserva la metadata `appearance` solicitada. Esto describe el fallback core observado; no demuestra que todas las superficies finales de Cursor HC-light se pinten oscuras, porque intervienen otras capas de resolución y estilos.

El mapa `Bli` conecta esos tokens con IDs de colores de VS Code. Por ejemplo, `sidebar` usa `sideBar.background`, `chrome` usa `panel.background`, `editor` usa `editor.background`, `base` usa `editor.foreground`, `accent` usa `button.background` y `actionLabel` usa `button.foreground`. Los colores de Git usan `gitDecoration.*ResourceForeground`, los ANSI usan `terminal.ansi*` y los fondos de diff usan `diffEditor.inserted/removedLine/TextBackground`. Hay aliases deliberados: `success` y `terminalAnsiGreen` leen el mismo ID; `blue` y `terminalAnsiBlue` también. No son 43 claves independientes del sistema completo de VS Code.

## Formatos y validación observados

La ruta YAML `_parseCursorCoreThemeYaml` comienza en `31456136`. Usa el loader YAML, exige un mapa y un `name` de texto con longitud mínima uno. Admite `appearance`, `highContrast` y los tokens core como campos del mapa superior. Las opciones ausentes se completan con el bucket de defaults. Los tokens de tipo distinto de string se omiten; un string vacío después de recortar whitespace tampoco reemplaza el default. Un string de color inválido provoca un error de importación. El editor añade una comprobación de nombre recortado no vacío antes de guardar.

Las utilidades en `31424797` y `31424940` aceptan los aliases `light`/`vs`, `dark`/`vs-dark`, `hc`/`hc-black` y `hc-light`. Una apariencia desconocida cae a oscuro en esta implementación de Cursor. Un `highContrast` booleano explícito prevalece; si falta, puede inferirse de un tipo que empiece por `hc`.

La normalización acepta colores compatibles con su parser CSS y una ruta específica para OKLCH, y produce hexadecimal con alfa cuando corresponde. No se verificó en runtime el conjunto completo de sintaxis CSS aceptadas. El serializador YAML de `31456810` exporta `name`, `appearance`, `highContrast` y los 43 tokens, con indentación de dos espacios y sin ajuste de líneas.

El importador de archivos JSON `wa1`, en `31426451`, usa `JSON.parse`. Por ello, **esta ruta de archivo requiere JSON estricto**: no incorpora un parser JSONC. Su esquema raíz `M9g`, en `31428619`, es permisivo con campos adicionales y con los tipos de `name`, `type` y `colors`. Si no encuentra un objeto `colors` válido, utiliza un mapa vacío y conserva los defaults. Un nombre ausente o inadecuado recibe el fallback de nombre del importador. Sólo extrae los IDs mapeados a los 43 tokens.

En esa conversión de archivo no se procesan `tokenColors`, reglas de sintaxis TextMate, `semanticTokenColors` ni `include`; no se recorren archivos referenciados. Eso limita la fidelidad de importar un tema de VS Code como tema core. No permite concluir que Cursor o VS Code carezcan de esas funciones: la importación desde **temas ya instalados en el IDE** usa el servicio de temas y `getColor`, que puede entregar colores ya resueltos por otra capa. No se inspeccionó exhaustivamente esa resolución previa.

## Biblioteca, editor y archivos observados

La biblioteca se guarda bajo `glass.theme.customThemesV1` (`31431240`), mediante el servicio de almacenamiento. Su envoltorio es `{_v: 1, themes: {...}}`. Cada registro conserva `id`, `name`, `source`, `bucket`, `definition`, `createdAt`, `updatedAt` y, opcionalmente, `vscodeThemeSettingsId`. Las procedencias observadas son `imported`, `vscode` y `user`. El cargador en `31457021` recupera registros válidos; una envoltura ilegible no se toma como una biblioteca válida. No se deduce de esta clave una ruta física a `settings.json`.

Los identificadores de selección usan `cursor-custom-theme:<id>`. El ID interno combina un slug ASCII de hasta 32 caracteres y un UUID. El servicio fija un máximo de 30 temas personalizados, con rechazo explícito al intentar crear otro registro (`31462995`). Actualizar un tema existente conserva su identidad y modifica su fecha de actualización.

Las operaciones del servicio están en `31461302`–`31463525`: eliminar, duplicar, crear, actualizar y exportar. Al borrar un tema seleccionado se restablece una selección incorporada; también se corrigen referencias del tema en los buckets. Los temas incorporados tienen acciones de aplicar, duplicar y exportar; su menú no ofrece editar ni eliminar (`31669238`–`31672440`).

El constructor del editor (`31758512`–`31762112`) mantiene un borrador local: un tema nuevo parte del tema resuelto actual con nombre vacío; editar parte del registro guardado. Presenta nombre, claro/oscuro, alto contraste y controles de color para los tokens. La entrada de color usa hex de seis u ocho dígitos según el alfa del campo. Guardar valida el nombre recortado, llama a crear/actualizar, aplica el resultado y cierra el diálogo. Cancelar cierra el borrador sin llamar a esas operaciones. No se observó en este flujo un guardado automático de cada cambio de color.

La paleta de gestión permite buscar y agrupar por `light`, `dark`, `lightHighContrast` y `darkHighContrast`. La opción de temas avanzados depende de `glass_custom_theme_support`; la presencia del código no acredita que el flag esté activo para toda cuenta.

El selector de importación de `31675294` permite un único archivo local y anuncia extensiones `.yaml`, `.yml` y `.json`. Decide la ruta JSON por el sufijo `.json`; el resto pasa a YAML. La importación desde el IDE enumera temas importables, pide confirmación para importar el conjunto e informa éxitos y errores. No se midió la disponibilidad efectiva de extensiones instaladas.

La exportación de `31669439`–`31669764` propone `userHome/Downloads/<slug>.yaml`; el slug de archivo usa minúsculas y guiones bajos, hasta 32 caracteres, con fallback `theme`. Es una ubicación sugerida en un diálogo de guardar: el usuario puede elegir otro destino o cancelar. No se observó aquí un tamaño máximo de archivo de importación equivalente al límite propio de openADE.

## Evidencia focal reproducible

Los siguientes intervalos contiguos del bundle original permiten repetir la inspección sin incluir sus bytes en este repositorio. Los recortes son fronteras elegidas por el analista; no todos son módulos JavaScript autónomos.

- Tokens: `[18514508, 18515326)`, SHA-256 `2904721566073f7e5318c3353b3e451f8e0ef7e85eb0caacd197b533041f3847`.
- Defaults core: `[18540651, 18546256)`, SHA-256 `f74a6c4f5d69758acfc273c1e22deb77245d850c4634b5813ad64800b91c0bf4`.
- Utilidades de formatos e identidad: `[31424712, 31428802)`, SHA-256 `f7caedaad1dbd7324851fcdbd19e2ab72370ef5f27d6431915e567aaaeadb5f2`.
- Parser y almacenamiento: `[31455903, 31458235)`, SHA-256 `1ea32cb3c858151ba9f4fbdc5ed7cce107890a8ae89bc1b328a33585600aa3db`.
- CRUD: `[31461302, 31463525)`, SHA-256 `3ad7ef0b384ef9eee68d65d8f216650dec5f5a0a6f7e348d9e8b976088822ee6`.
- Paleta, importación y exportación: `[31669238, 31679182)`, SHA-256 `83c9d9d336bdf906395dd90f1049d822b368cfaad9c5d46825cfe89aa4ca5572`.
- Constructor y guardado del editor: `[31758512, 31762112)`, SHA-256 `b6ac4839ce056510682c8127c4e7e95d6e5d78bf0da4c39adc61a02fb02d3535`.

## Adaptación propia de openADE

El modelo [custom_theme.rs](../../../apps/agents-window/src/custom_theme.rs) conserva los 43 tokens para intercambio, persistencia y exportación. El painter nativo resuelve **18 grupos efectivos**, que son los que permite editar la UI: `sidebar`, `chrome`, `editor`, `base`, `accent`, `actionLabel`, `brand`, `focus`, `warn`, `danger`, `added`, `removed`, `blue`, `purple`, `green`, `orange`, `diffAddedLineBackground` y `diffRemovedLineBackground`.

Los otros **25 campos se conservan y se exportan**, pero no tienen una función de pintura independiente en el renderer actual: `success`, `modified`, `untracked`, los 16 ANSI, `red`, `yellow`, `cyan`, `magenta`, `diffAddedTextBackground` y `diffRemovedTextBackground`. El importador informa esta limitación. Conservar esos valores no equivale a aplicar una paleta ANSI completa al terminal ni a reproducir todas las distinciones de Cursor. Los aliases de los colores nativos comparten grupos; por ejemplo, varias tarjetas y campos derivan de `sidebar`, y el texto secundario deriva de `base`.

Los aliases también alcanzan los colores de sintaxis y las marcas/fondos de los
CodeDiff y las revisiones nativas de limn. Los fondos conservan su fuerza relativa:
el diff usa un tinte de 20/28 de la revisión del editor. La opacidad configurada
define el máximo de esa escala y las transiciones lo multiplican continuamente;
no se deduce el tipo de vista a partir de una opacidad que ya esté animada.

El **alto contraste claro de openADE es una adaptación propia**: conserva superficies claras, texto y foco oscuros y colores semánticos adecuados para esa base. No copia el bucket oscuro de alto contraste observado como fallback de Cursor. Los overrides explícitos del tema siguen siendo datos del usuario. Esto evita convertir una selección clara en oscura, sin afirmar identidad visual ni una certificación de contraste del runtime de Cursor.

La biblioteca nativa vive en `appearance.custom_themes` de `settings-preferences.json` dentro de la raíz de datos de openADE, junto a su versión y selección activa. Mantiene el máximo de 30. Añade validaciones propias: archivo local regular, UTF-8, importación de hasta 1 MiB, nombres no vacíos de hasta 120 bytes sin controles y colores de hasta 256 bytes que se normalizan a sRGB hexadecimal. Un formato o tipo inválido produce un error antes de guardar; esta política es más estricta que las omisiones toleradas por los parsers de Cursor descritas arriba.

La importación nativa acepta YAML core o JSON estricto de colores de VS Code, abre un borrador revisable y no carga `include`, URLs, archivos auxiliares ni reglas de sintaxis. Los campos core desconocidos no se convierten en controles arbitrarios. Exporta YAML con los 43 tokens. El flujo usa los selectores de archivos disponibles del host; no reproduce el catálogo de extensiones y el servicio de temas instalados de Cursor.

Las pruebas y la aceptación de la implementación nativa se registran en el informe general de integración. Este relevamiento estático, por sí solo, no acredita ejecución o paridad visual en el runtime propietario.
