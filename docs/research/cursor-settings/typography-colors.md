# Tipografía, tinte y alto contraste de Cursor

Se inspeccionó el mismo bundle Glass de Cursor 3.17.21 identificado en [README.md](README.md), SHA-256 `b6f06229dc965619ab318e3bbc9f102865101a3bc0f346630532e2558f6fc398`. Los offsets son **bytes UTF-8**, con origen cero. Son hechos del código distribuido; no se ejecutó Cursor ni se pidió acceso a las fuentes del equipo mediante su runtime.

## Familias: las opciones se consultan al abrir el menú

El componente de familias `v8g` comienza en `31492789`. Al abrir el menú limpia la búsqueda y llama a `N_g`; la lista no está escrita como un catálogo fijo de tipografías. La consulta de bajo nivel usa `queryLocalFonts()` (`3021952`–`3022194`) y recupera familia, nombre completo, nombre PostScript y acceso opcional al blob. Los errores se registran y se convierten en una lista vacía.

`N_g` (`29338952`) comparte una promesa de descubrimiento. Si la consulta devuelve cero fuentes, invalida esa caché para poder reintentar al abrir de nuevo. Si devuelve fuentes, recorta nombres, elimina duplicados por familia sin distinguir mayúsculas y ordena el resultado. Filtra fuentes simbólicas, de iconos y emoji mediante nombres conocidos y, cuando dispone del blob, metadata OpenType. Las lecturas de blobs se acotan a cuatro tareas simultáneas; un blob mayor de 8 MiB se acepta sin la inspección completa de su contenido en esta ruta. Estos filtros no equivalen a instalar ni descargar una fuente.

La clasificación monoespaciada compara los anchos de `i`, `l`, `W`, `M` y `1` mediante un canvas a 16 px, con tolerancia de 0,01 (`29340069`). Esta es una prueba de medición del renderer de Cursor; no es el atributo monoespaciado de una base de fuentes nativa.

El menú (`31492789`–`31496404`) ofrece:

- Búsqueda por substring del nombre de familia, sin distinguir mayúsculas, con foco inicial en el buscador.
- En interfaz: grupo **All Fonts**.
- En código: grupos **Monospace** y **Other Fonts**. Cursor permite elegir también familias proporcionales para código; no limita esta lista a monoespaciadas.
- Sin búsqueda: entrada predeterminada y, cuando existe un override, entrada de la selección persistida. Esto permite representar una familia guardada que ya no esté en el inventario del equipo.
- Vista previa del nombre de cada familia usando esa familia, check en la selección, estado de carga y mensaje de ausencia de coincidencias.

La lista está virtualizada, tiene ancho de 280 px y altura máxima de 320 px; la fila base es de 24 px. Elegir una entrada confirma su valor y cierra el menú. El selector no ofrece aquí una caja para introducir libremente un nombre nuevo: el texto de búsqueda filtra el inventario. El valor de la familia se convierte a una expresión CSS entre comillas cuando lo necesita.

Los labels predeterminados de la pantalla son **System font** para interfaz y **System monospace** para código (`31497980`–`31501856`). El default elimina el override. El servicio recorta el valor, limita la familia a 500 unidades de string de JavaScript y rechaza delimitadores o expresiones CSS peligrosas, así como familias identificadas como simbólicas (`Cas`, `29341442`). No se verificó la concesión de permisos de `queryLocalFonts` ni el inventario efectivo en el sistema del usuario.

## Tamaños, persistencia y preview de código

La pantalla presenta controles numéricos con paso uno, escritura, decremento/incremento y restablecimiento (`x8g`, `31497010`). Mantiene el valor local mientras se edita y lo manda al servicio al confirmar. El servicio normaliza al entero admitido más cercano; los límites de UI son 7–38 y los de código 10–22. Los defaults son 13 y 12, respectivamente (`25022159`, `25022166`; esquema de opciones en `29336645`–`29336708`).

El servicio de display (`30863160`–`30867939`) guarda un objeto JSON bajo `glass.display.settings`: `uiFontSizePx`, `codeFontSizePx` y los overrides opcionales `uiFontFamily` y `codeFontFamily`. Leer un objeto inválido o JSON ilegible lleva a sus defaults. Al cambiar una familia persiste y emite un evento; al cambiar tamaños actualiza las ventanas registradas y emite el mismo tipo de notificación.

El tamaño de interfaz controla el zoom de la ventana. El cálculo observado usa 100 % en 13 px y pasos de ocho puntos porcentuales por unidad; el tamaño de código se compensa respecto de ese zoom. No debe interpretarse el tamaño de código como otro zoom global.

**Tras las dos familias hay un preview de diff TypeScript**, no sólo los selectores. Su figura tiene el label accesible `Sample diff preview for the code font` (`31497980`–`31501856`). Usa las medidas tipográficas resueltas para el código, gutter y altura de línea; presenta contexto y números de línea y fija `wordWrap: false` en este ejemplo. Esto permite comprobar la fuente del editor y de los diffs antes de abandonar Settings. No se midió la apariencia final de los glifos en el renderer propietario.

Font Smoothing es otra fila de Typography, visible sólo en macOS en el código de Cursor. Su descripción controla antialiasing nativo de macOS; la colocación subpíxel del renderer nativo de openADE es una adaptación distinta, descrita en [appearance-completion.md](appearance-completion.md).

## Por qué Hue puede verse gris

El estado inicial del tinte es `hue: 210, intensity: 0` (`31441607`–`31441615`). Las claves persistidas son `glass.theme.customTintHue` y `glass.theme.customTintIntensity` (`31431139`, `31431171`). Hue recorre 0–360; Intensity, 0–100 %.

La pantalla deriva `active` de `intensity > 0` y pasa esa bandera al control Hue (`31506781`). En el componente Hue (`17939322`), `active: false` lleva la saturación visual a cero y deja el swatch sin un color de fondo propio. **Ese gris no significa que falten colores o que la lista esté vacía.** El estado deshabilitado es una propiedad distinta; el slider conserva su handler. En intensidad cero, la capa de tinte se vacía y no altera las superficies (`31454409`). Cambiar únicamente Hue a 0 % puede producir ningún cambio visible en la aplicación.

Los controles de Hue e Intensity son sliders, no menús de una paleta de colores. Hue presenta un swatch y el control de intensidad muestra un porcentaje. Sus componentes redondean y acotan los valores; la fila de Settings usa callbacks separados para cambio local y confirmación.

Durante el arrastre, `onChange` actualiza el estado local de esos controles. La ruta observada sólo llama a `setCustomTint` desde `onCommit`, mediante un scheduler de 250 ms (`M8g`, `31512436`). `setCustomTint` actualiza la capa de estilos y pospone su persistencia 300 ms (`31441152`, `31463961`). No se atribuye a Cursor un preview global de todas las superficies en cada evento de arrastre: esa conclusión no se sigue de estos handlers.

El overlay opera sobre cinco tokens: `sidebar`, `chrome`, `editor`, `accent` y `focus` (`31431223` en adelante). Sidebar recibe la intensidad; chrome/editor reducen su escala cromática a la mitad. Accent/focus cambian su hue conservando la saturación y luminosidad de origen. No es una sustitución de toda la paleta del tema, ni un recoloreado de imágenes o páginas web. Los colores arbitrarios del editor de temas personalizados son otra función, documentada en [custom-themes.md](custom-themes.md).

## La sección High Contrast es independiente

Después de Typography, la pantalla monta `cl1`, cuyo componente comienza en `31489960`: título **High Contrast**, fila **Follow System High Contrast**, descripción de cambiar a un tema de alto contraste cuando el sistema esté en ese modo. La acción llama a `setDetectHighContrast`; no modifica el tamaño o la familia del editor.

El default local de Glass es **true**: `m9g = !0` en `31415963`; `_loadPreference` lo usa al leer `glass.theme.detectHighContrast` en `31442161`. El ajuste del IDE asociado, `window.autoDetectHighContrast`, también registra default **true** (`EmC`, `39902136`). Cuando Glass está sincronizado con el IDE y su estado ya se inicializó, muestra el valor efectivo del IDE. Si una política administra ese ajuste, deshabilita el switch (`31436769`–`31437107`). El default del código no garantiza el estado efectivo de una instalación existente.

Además hay selectores de tema para **High Contrast Light Theme** y **High Contrast Dark Theme**. Aparecen cuando el seguimiento de alto contraste está activo y la pantalla permite los pickers avanzados de temas, condicionados por `glass_custom_theme_support`. El seguimiento del tema claro/oscuro y el seguimiento de alto contraste tienen flags y preferencias separados.

El primer inventario conservaba el catálogo de búsqueda y describía alto contraste de forma general, pero no desarrollaba individualmente esta fila ni su default. Este documento completa ese detalle. No se comprobó la señal de alto contraste del sistema operativo en runtime. openADE ofrece variantes manuales propias; no debe presentar como funcional un seguimiento automático del host mientras no tenga una fuente real para esa señal.

## REA focalizado y límites

Se reutilizó la proyección previa de Typography/Appearance. Para responder las preguntas nuevas se analizó un directorio temporal de seis recortes contiguos exactos, 49.497 bytes contando su manifiesto. REA parseó los seis archivos JavaScript sin fallos, visitó 11.475 nodos AST y conservó 2.786 fronteras semánticas desconocidas. El código se trató como texto inerte: no se arrancó Electron ni se consultaron fuentes del host por medio de Cursor.

- Análisis retenido: `ev_4e00aa3564030e9e7f9f299cf231ad2c0cc5b69d1ccfe532602c38ec52d9b819`.
- Vista resumen: `ev_28bbb6cd633a9f5ecaa14659265593901b9b0eecbb3738a93960ce93d72187b2`.
- Vista del módulo de consulta: `ev_72a91b7a5b4173745082944a646872f688e7feb8563b6202846c8c9693de2610`.
- Vista del picker: `ev_b584467fd97529f4eceb31345778e0b25e85d902b4b83553d7a9eeeb8172b021`.
- Traza de `uiFontFamily`: `ev_f1ea5fe2c75c2a78d031a2c279a9dcbc91794c15111e4303390346cbec276947`, ambigua por receptores externos no resueltos.
- Traza de `detectHighContrast`: `ev_daee35f2670d4c7c8c09cccaa9571c79fc0d21fd537d06796922fccba374ffc2`, ambigua por la configuración dinámica suministrada al componente.

Los hechos funcionales anteriores se sustentan en los cuerpos y sus condiciones observadas; las trazas no acreditan ejecución ni resolución entre todos los servicios minificados. Un seed sin coincidencias en una traza parcial no se utiliza como prueba de ausencia de una función.

Recortes y SHA-256 verificables contra el bundle original:

- Consulta: `[3021952, 3022194)`, `20b4cbfa7271c9eceb641925791b82040dfa348a37226c3b825ca59a13b51557`.
- Descubrimiento y clasificación: `[29336925, 29340475)`, `5588ab1bcc7cf87d53a49c9f7eeca917410ac217a4ce37b0001bd3485b37eb8c`.
- Picker de familias: `[31492789, 31496404)`, `b60b036228f62dc13a47cebb7566250e0645624a42f629e6641b87bc1bf8c27a`.
- Servicio de display: `[30863160, 30867939)`, `14b5d4d6ff35ead8609c0f510b2da01ab073fb45359a060ee8f59986f5181c87`.
- Servicio de temas/tinte: `[31429706, 31464342)`, `ff7382081739a11cc97d2d0dd76830634f2c7f0fe48d2e8a79cfcfbd54b2cfdb`.
- Sección High Contrast: `[31489960, 31491164)`, `92aafe26948758c907e9e3ad2d8b85bf3564f2e514224247b74f196eaa91f055`.

La lista exacta de fuentes instaladas, permisos de la API local, tipografías ausentes en otra máquina, políticas de cuenta/IDE y resultado visual GPU permanecen sin observar. Una lista vacía en openADE exige revisar su descubrimiento y carga de fuentes; no demuestra que Cursor carezca de opciones ni se resuelve con nombres de fuentes que el renderer no pueda cargar.
