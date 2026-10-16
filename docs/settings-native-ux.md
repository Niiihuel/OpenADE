# Settings nativo

La implementación usa los componentes de `openide-ui`. El clon de referencia
es `references/zcode`, revisión `29628c9acdb81b703bbd4080c207a0e7ce5e276e`.
Se consultaron `SettingsPageParts.tsx`, `SettingsResourceGroup.tsx`,
`model-provider-section/SectionLayout.tsx`, `Navigation.tsx` y `DESIGN.md`.
El código de presentación es Rust propio.

## Estructura común

Las veinte rutas comparten títulos, descripciones, grupos, filas con separadores,
controles del framework y una columna centrada de hasta 960 px. Los márgenes de
12/20 px acompañan la densidad del IDE. La navegación y los controles usan
28 px; las filas tienen 10 px de relleno y los grupos 8 px de radio. Los
títulos de página son de 18 px, las etiquetas de 12 px y las descripciones de
11 px. Los estados vacíos usan una tarjeta breve que crece con su contenido. Los controles se
apilan cuando el contenido disponible baja de 640 px; los switches mantienen
su posición junto a la fila. La navegación tiene
búsqueda por título o descripción; en superficies pequeñas se abre desde
Secciones. `Ctrl+K` enfoca la búsqueda de modelos dentro del catálogo y la
búsqueda de ajustes en las demás rutas.

General, Apariencia, Terminal y Datos editan las preferencias existentes.
Agente, Skills, MCP, Comandos y Hooks conservan las acciones de sus servicios,
incluidos formularios, permisos y recuperación. Automatizaciones y GitHub
conducen a sus superficies conectadas. Subagentes configura la delegación y sus
límites para próximos turnos, muestra los roles disponibles y conserva el estado
durable de la sesión seleccionada. Navegador configura automatización, enlaces,
vistas previas y permisos por sitio. MCP separa Usuario y Este proyecto, con
importación local e inspección sobre el ámbito elegido. El
[contrato de integración](settings-module-integration.md) detalla persistencia,
revocación y recuperación. Memoria y Uso distinguen datos actuales de capacidades
todavía no agregadas.

Apariencia incorpora temas claros/oscuros y de alto contraste, tinte con preview,
transparencia, densidad de herramientas, wrap de código, fondos de diff,
tipografía independiente para interfaz/código, familias instaladas, movimiento
y máscara de correo. Usa las propiedades nativas de limn propuestas en
[PR #43](https://github.com/Niiihuel/limn/pull/43), fijadas por revisión en Cargo.
El [relevamiento de Cursor 3.17.21](research/cursor-settings/README.md) conserva
fuentes, evidencia REA, defaults y límites de paridad con el producto de referencia.
Plugins administra paquetes locales, con revisión previa, instalación inactiva,
activación por proyecto, detalles y desinstalación. El contrato de ejecución y
sus límites se documentan en [Plugins](plugins.md).

La navegación distingue las capacidades con iconos propios (usuarios, pieza,
servidor y libro para Subagentes, Plugins, MCP y Skills). Los badges informativos
conservan su altura de 22 px al envolver los encabezados. Los diálogos usan
título de 16 px, cierre X de 24 px y botones de 28 px; Escape cierra sin
confirmar y las acciones destructivas conservan Cancelar como foco inicial.

### Selectores y rendimiento

Agrupar tareas, Ordenar tareas y Aislamiento de comandos usan `DropdownList`
con el perfil `SelectorSizing::COMPACT`. El botón muestra el valor vigente y
el menú marca la opción elegida; Escape, clic exterior y cambiar de sección
lo cierran. Las acciones conservan la persistencia y los controles de
disponibilidad existentes. Los permisos independientes siguen usando switches.

El listado de modelos retiene el snapshot compartido del catálogo y prepara
una vez el adaptador y un índice de compatibilidad por ruta exacta. La lista
virtual crea las filas visibles; no vuelve a deserializar el registro de
proveedores por cada modelo ni copia sus metadatos completos por frame.
Mover el puntero, enfocar o desplazar una lista actualiza el estado visual de
`UiTree` sin construir otra vez Settings. Los cambios de valores o resultados
asíncronos reconstruyen la vista cuando corresponde. Se conservan hover,
animaciones, tooltips, selección de texto y barras de desplazamiento.

La regresión compara el `Frame` completo con la implementación anterior a
360/900 px, incluidos búsqueda, hover, foco, selección y scroll. El benchmark
optativo `benchmark_provider_model_list_635_models` usa 635 modelos y rutas
sintéticos, sin red ni credenciales. En debug, 20 iteraciones por caso midieron
aproximadamente 102 ms antes y 1,2 ms después para preparación y layout visible.
Es una medida CPU del componente; no representa FPS ni tiempo de pintura GPU.

Las tarjetas relacionadas de subagentes indexan los IDs activos y guardados una
vez. Mantienen los vectores originales para conservar orden, duplicados,
aprobaciones y recuperación; sus comprobaciones de presencia ya no hacen
búsquedas cruzadas por cada tarjeta. La validación del harness conserva su
autoridad antes de ejecutar herramientas.

## Agente, Memoria y Subagentes: motor conectado

Agente presenta el harness propio de OpenIDE y su perfil nativo guardado.
«Elegir modelo» conduce al selector real del composer; no cambia el modelo
por abrirlo. El proveedor y el ID del modelo nativo aparecen completos, con
credencial ausente, detectada o conexión sin clave separados de la aceptación
de generación. La UI no infiere capacidades por nombre de modelo.

Las nuevas tareas usan sólo el harness nativo, incluso si la preferencia vieja
seleccionaba una CLI instalada. El historial externo conserva su motor y sus
eventos; ofrece «Nueva tarea nativa» y no admite otro turno desde el composer.
Las acciones obsoletas del selector de motor se ignoran. El selector de modelos
presenta únicamente conexiones disponibles con sus modelos; no muestra
proveedores sin credenciales ni conexiones pendientes/fallidas. La gestión y
conexión de proveedores permanece en Settings → Proveedores y modelos. Un
proveedor local que no exige clave conserva su ruta configurada válida.

Sin una ruta disponible, el chip muestra «Sin modelos» en superficies estrechas
y «No hay modelos disponibles» en las amplias; el menú conserva ese estado vacío; el perfil interno predeterminado no se promociona como
modelo elegido. El botón «Conectar proveedor» permanece en las vistas del
selector, incluso cuando ya existen modelos. Conduce a Settings → Proveedores
y modelos → Conexión sin elegir modelo, alterar credenciales, admitir un turno
ni perder el borrador. En reposo, Enviar queda deshabilitado; `start_run` también
rechaza el perfil implícito antes de preparar archivos o admitir un turno. Los
controles activos de Stop/Steer conservan su contrato. Las conexiones sin clave
se determinan por el adaptador registrado y la resolución válida del endpoint,
incluidas las descubiertas; no por una lista de IDs de proveedores. Agente oculta
también el perfil implícito cuando no hay candidato exacto y conserva los IDs
completos y el estado de las rutas explícitas, incluso con credencial ausente
o capacidades negativas.

## Contexto y actividad en el chat

El control Contexto reemplaza el selector Nativo/Codex/Claude del pie del composer.
`ContextMeter` muestra uso de la última respuesta atribuible a la ruta exacta;
sin medición presenta «Contexto», no 0%. `ContextUsage` abre una tarjeta de hasta
320 × 200 px lógicos, con barra fina, porcentaje/cociente y entrada/salida.
«Ver detalles» expande `ContextContent`, con las categorías y scroll
para límites del modelo, entrada/salida/total, cache/razonamiento, cobertura de
uso acumulado, snapshots y archivos actuales de instrucciones/skills, identidad
MCP y metadatos inspeccionados. Los bytes y cantidades no se convierten en tokens.
La consulta valida dueño/proyecto/replay/checkpoints y no ejecuta herramientas.
La procedencia y los límites están en `docs/native-context-projection.md`.

`Marker` y `TextShimmer` presentan thinking, respuesta incremental, herramientas,
permisos y checkpoints desde sus eventos reales. La compactación manual sólo
anima durante su worker; un checkpoint requiere confirmación durable. Git muestra
cambios observados entre revisiones del mismo ámbito, conservados en RAM.
Movimiento reducido mantiene texto íntegro sin animación. Ver contratos y
límites en `docs/conversation-markers.md`.

Las referencias visuales son [Context de AI Elements](https://elements.ai-sdk.dev/components/context)
y [Marker de Rust UI](https://rust-ui.com/docs/components/marker). La presentación
y la proyección de datos se implementan en Rust con `openide-ui`.

## Permisos, memoria y coordinación

El switch «Escritura y comandos» afecta exactamente esos dos permisos. Leer,
listar y buscar siguen siendo independientes. Los permisos se guardan para
próximos turnos; los cambios de aislamiento y la revocación de comandos cierran
los procesos persistentes existentes. Plan sólo anuncia lecturas certificadas.
El soporte de la build se consulta a `CommandIsolation::is_supported_on_host`;
`is_available_on_host` también exige el helper compatible en Linux. Settings,
el catálogo persistente y la política consultan esa autoridad: una selección
guardada no disponible conserva el perfil y bloquea comandos, sin cambiar a
Host. Si falta Bubblewrap, Linux presenta el requisito concreto y deshabilita
esa opción. Cada proceso debe completar además la preparación real del sandbox
antes de liberar su ejecutable; detectar el helper no certifica el equipo.
El [contrato Linux](linux-command-sandbox.md) detalla los límites de runtime,
archivos, red y proceso. El estado por defecto existente se conserva.

Memoria presenta contexto estimado de la última respuesta, historial/checkpoints,
instrucciones congeladas y goal de la conversación. No anuncia recuerdos
aprendidos entre conversaciones. El goal conserva texto, criterios y evidencia
íntegros con scroll; abrirlo vuelve al editor visible de la tarea. Compactar
invoca el servicio existente, que revalida sesión nativa, reposo, lease, replay,
historial y conexión. Los errores se muestran en Settings; una operación iniciada
vuelve a la tarea. La prueba
`settings_memory_compacts_real_http_turn_and_reopens_checkpoint_and_goal`
activa un goal, ejecuta un Plan por HTTP local y hace clic en Compactar: verifica
petición sin herramientas, marcador durable, reapertura del checkpoint y ledger
sin evidencia inventada. No demuestra inferencia externa, ciclo autónomo ni
cortes de proceso del desktop.

«Revisar copias» consulta el journal validado y los checkpoints de la conversación
seleccionada, incluso para turnos terminados. El selector muestra una copia
completa de las instrucciones adicionales iniciales (proyecto, skills y goal),
con dueño, secuencia, fecha, versión, digest y scroll. No sustituye el texto por
los archivos actuales ni reconstruye el wrapper del motor o los preflights de
directorios. La prueba HTTP anterior también verifica la inclusión literal de
esa copia en la petición real, su igualdad con el evento y su conservación tras
reapertura sin escribir eventos nuevos. Cinco pruebas del inspector cubren
validación, corrupción, ámbitos, controles, foco y texto Unicode completo.

Skills presenta el documento completo descubierto con su procedencia y digest.
Aceptar una selección vuelve a validar el contenido en disco. Las selecciones
obsoletas pueden vaciarse sólo para el proyecto actual, con confirmación sobre
una copia exacta; un cambio externo entre apertura y confirmación bloquea el
guardado. Esto conserva las selecciones de otros proyectos y no borra SKILL.md.
El documento de vista previa permanece en RAM y no se persiste como preferencia.

Subagentes reutiliza las tarjetas del motor: estados durables, permisos pendientes,
Resume, cancelación, entradas completas y edición/omisión por revisión. El
recuento pertenece a la conversación seleccionada. Abrir tarea navega entre
padre e hijo, y Ready ofrece iniciar explícitamente con el dispatcher existente.
Los perfiles especializados y límites de concurrencia aún no tienen editor.

Comandos incluye una evaluación del archivo de reglas guardado sobre un argv
JSON exacto. Presenta decisión, coincidencias, intérpretes opacos y digest sin
ejecutar el comando ni otorgar permisos. Conserva los borradores; las aprobaciones
y permisos efectivos siguen perteneciendo al motor durante la ejecución.
Las pruebas de acciones/reapertura comprueban cambios externos, corrupción y
ausencia de ejecución; las vistas verifican contenido completo y controles a
360/900 px.

## Terminal

La tarjeta de Terminal tiene un único selector de shell predeterminado, con
la opción guardada y una marca de selección en el menú. Los presets se validan
antes de persistir. “Personalizado…” abre el campo dentro de esa misma tarjeta
y lo enfoca; la selección guardada se mantiene hasta comprobar y guardar el
ejecutable. Guardar y Enter rechazan rutas vacías o relativas; Cancelar restaura
la ruta guardada tanto en el estado como en el editor retenido del árbol.
La ruta personalizada activa permanece visible al volver a esta sección y,
durante una edición, se conserva también la referencia a la ruta guardada.
El texto aclara que sólo las terminales nuevas reciben el cambio.
El selector sigue el patrón de opciones mutuamente excluyentes de
[GNOME HIG](https://developer.gnome.org/hig/patterns/controls/drop-downs.html);
el botón de guardar queda deshabilitado si falta una ruta absoluta, siguiendo
su [guía de botones](https://developer.gnome.org/hig/patterns/controls/buttons.html).

## Providers y modelos

Las acciones pendientes usan el spinner circular animado compartido mediante
`Button::loading(...)` o `IconButton::loading(...)`. Se mantiene la etiqueta de
la acción y se bloquean activaciones repetidas hasta terminar. Los puntos
suspensivos no sustituyen este indicador. Los skeletons se conservan para
contenido cuya estructura todavía está cargando. La animación respeta la
preferencia de movimiento reducido.

Los grupos de preferencias aplican su radio de 12 px también al hover y a la
presión de las filas exteriores; las filas interiores tienen bordes rectos.

El catálogo presenta grupos de cuentas conectadas y disponibles con filas de
acción del framework propio. La vista actual ofrece Codex, OpenRouter, OpenCode
Zen/Go y NVIDIA NIM. Cada fila abre la cuenta, sus modelos y los controles de
conexión. Se usan OAuth en el navegador para Codex/OpenRouter y campos de clave
enmascarados para Zen/Go y NIM. El registro determina endpoint, protocolo y fuente
de listado; el catálogo nunca suministra una credencial o endpoint de inferencia.
Ver [flujos y almacenamiento](provider-connections.md).

Explorar, filtrar y abrir detalles no altera la ruta del agente. La lista
virtualizada permite recorrer todos los modelos de la fuente, sin recortarlos.
Se conserva el ID exacto junto al nombre; Detalles muestra
modalidades, límites declarados, precio de referencia, esfuerzos y fuente en
un modal con scroll y retorno del foco.
Los datos ausentes permanecen desconocidos. Las fechas corresponden a la última
comprobación o intento real; un refresh fallido conserva la última copia válida
y presenta el error.

Usar modelo guarda el perfil nativo y activa el backend nativo en el composer.
La credencial detectada, el listado verificado y el acceso de generación se
presentan como hechos distintos. Los negativos explícitos de herramientas o
texto y las exclusiones del listado autenticado bloquean la ruta exacta; una
ruta nueva sin esos negativos permite la elección explícita sin anunciar
generación aceptada.

El editor de salida permanece vinculado a la ruta activa, aunque se explore otro
proveedor. Codex no ofrece este editor porque la ruta del plan de ChatGPT no
admite topes propios. Las otras rutas conservan los parámetros explícitos de Chat Completions, validación,
persistencia y bloqueo durante un turno. La presentación muestra metadatos de
límites; el presupuesto efectivo sigue resuelto por el host.

Las credenciales se guardan en archivos privados de la cuenta local y no se
copian al estado de presentación. Los providers por clave mantienen el entorno
como fuente compatible si no existe una conexión local. El registro abierto y
los adaptadores anteriores permanecen en el motor; la ruta personalizada y
`gpt-4.1` dejan de formar parte del catálogo visible.

## Revisión visual

Las pruebas recorren todas las páginas a 360, 620 y 1300 px, verifican límites
horizontales, scroll y acciones de teclado, y alcanzan el último ID de un
catálogo de 300 modelos. Las regresiones de `openide-ui` cubren la altura real
del texto en filas flexibles y el crecimiento de estados vacíos para conservar
su botón dentro de la tarjeta. La exportación opt-in
`export_settings_visual_review` genera SVG de los comandos del árbol en
`target/settings-visual-qa`, usando los mismos iconos nativos. Esta exportación
permite revisar geometría, clips y orden de pintura; las fuentes y el blur
pueden diferir del renderer egui. No sustituye la aceptación de inferencia con
cuentas de proveedores reales descrita en el handoff.

## Controles y separación del scroll

Los controles de Settings usan una paleta local: relleno `#373737`, borde
`#4e4e4e` y texto `#ebebeb`. Así el botón se distingue de la tarjeta `#2b2b2b`
sin esperar al hover. Las opciones seleccionadas y los switches tienen estados
visualmente diferentes. El proyecto activo conserva su carácter informativo.
La navegación y los enlaces de ayuda mantienen su tratamiento discreto.
Las filas del lateral usan `settings_page:<destino>` y los enlaces del
contenido usan `settings_link:<destino>`. Estos últimos despachan la misma
acción de navegación mediante `dispatch_as`, con estados de hover, transición
de color y foco independientes. Compartir el ID visual mezclaba el relleno
del botón con el de la fila y dejaba varias entradas resaltadas en reposo.
Los textos secundarios de Settings usan `#a0a0a0`: sobre la tarjeta, su
contraste pasa de aproximadamente 4,0:1 a 5,4:1, sin cambiar el token global
de texto secundario de la aplicación.
Los controles laterales se centran verticalmente en su fila, incluso cuando
la descripción ocupa varias líneas. En Agente, «Escritura y comandos» tiene
un solo switch para habilitar o deshabilitar escritura y comandos juntos.
La descripción comunica el estado y las combinaciones personalizadas; los
switches individuales permiten ajustar cada permiso. Se eliminó la etiqueta
con aspecto de botón que competía con ese control.

El scroll de navegación y de páginas reserva un carril de 14 puntos lógicos.
El thumb de 5 puntos queda separado por 4,5 puntos del borde del contenido;
los hovers y los clics de las filas terminan antes del carril. El contenido
centrado descuenta ese espacio también en ventanas pequeñas.

## Investigación de legibilidad del texto

Revisión del 29 de septiembre de 2026. Esta aplicación usa `eframe/egui 0.36.2`
y `wgpu`; no usa Chromium ni WebKit. La medición del árbol y la pintura usan
las mismas fuentes del contexto egui. El renderer no transforma el texto en
una imagen de baja resolución para ampliarla después.

El código de la dependencia confirma que la familia proporcional predeterminada
prioriza **Ubuntu Light**. OpenIDE ahora incorpora **Ubuntu Regular**, de la
misma familia, para los textos Sans. Se conservan la fuente monoespaciada, las
fuentes de respaldo y los emojis. La fuente y su licencia se distribuyen en
`apps/agents-window/assets/fonts`, con revisión y hash del upstream. El cambio
mejora el peso de los trazos; no demuestra por sí solo equivalencia con Chromium.
La definición de fuentes está documentada en el
[código de epaint](https://docs.rs/epaint/0.36.2/src/epaint/text/fonts.rs.html),
y el archivo procede del
[repositorio de Google Fonts](https://github.com/google/fonts/blob/a50de97857f6626d6628c91e6a34c12ee8a16ede/ufl/ubuntu/Ubuntu-Regular.ttf).

En esta versión, el hinting y el redondeo del origen del texto a píxeles físicos
ya están activados. No corresponde aplicar el diagnóstico de versiones antiguas
de egui que no tenían hinting. También hay binning horizontal de glifos en
fracciones de píxel: favorece el espaciado y puede suavizar los trazos. No es
antialiasing RGB del panel. Se mantienen los valores de egui, sin forzar DPI,
MSAA, filtros nearest ni un hinting más agresivo sin una comparación controlada.
Los modos y sus compromisos están descritos en
[TextOptions](https://docs.rs/epaint/0.36.2/epaint/text/struct.TextOptions.html)
y [SmoothHinting](https://docs.rs/epaint/0.36.2/epaint/text/struct.SmoothHinting.html).

Chromium usa Skia. El rasterizador, la fuente, su peso, el espaciado, el ajuste
al píxel y la conversión de cobertura de los bordes pueden producir resultados
distintos aunque ambas aplicaciones tengan aceleración GPU. Skia documenta
estas diferencias de composición en
[The Raster Tragedy in Skia](https://docs.skia.org/docs/dev/design/raster_tragedy/).
El lenguaje Rust no determina esa calidad: Chromium también incorpora código
Rust de Fontations para su pipeline de fuentes, como muestra su
[seguimiento oficial](https://issues.chromium.org/issues/349952802).

La sesión inspeccionada usa Hyprland sobre Wayland, panel Samsung ATNA40CU05-0
a resolución nativa 2880×1800, 120 Hz y escala **2,00**. OpenIDE, Chrome y
ChatGPT se presentan como clientes Wayland nativos. La ventana de OpenIDE
1412×830 coincide con la captura original de 2824×1660 píxeles. No se encontró
reescalado fraccional, XWayland ni opacidad global reducida en esa sesión.
La previsualización de las dos capturas del chat usa tamaños diferentes;
compararlas allí no equivale a comparar sus píxeles originales a 100 %.

Algunos OLED tienen geometrías de subpíxeles que afectan los bordes del texto.
[Samsung explica esa relación](https://global.samsungdisplay.com/31411) para
sus monitores QD-OLED; esa fuente no identifica la geometría del panel de esta
notebook. No se debe asumir RGB, PenTile, WOLED o QD-OLED sólo por decir OLED.
Una captura PNG contiene el resultado del software, no la estructura física
del panel. Por eso, una diferencia que ya aparece en la captura apunta primero
a las fuentes y al renderizado. La pantalla puede influir en la percepción,
pero no hay evidencia para señalarla como causa principal en este caso.

La verificación final en el equipo debe hacerse con la app recompilada y
capturas originales al 100 %, comparando el mismo tamaño de texto. Los SVG
de revisión sirven para los botones y la geometría del scroll; usan otro
rasterizador y no pueden demostrar la nitidez final de egui/wgpu.
La comparación de fuentes se verificó además con egui-wgpu sobre Vulkan en
la Radeon 780M, a 2× y manteniendo los valores predeterminados del renderer:
Regular produce trazos más sólidos, conserva acentos y mantiene la misma
altura de línea a 12, 14 y 16 puntos. La frase de muestra se ensancha alrededor
de 2,1 %, por lo que se conserva la medición real de egui para ajustar el layout.


## Controles humanos de comandos

Actividad ofrece Controlar comando e Historial de controles para la sesión
nativa. La UI proyecta owner, generación, stdin y PTY observados, y prepara una
confirmación exacta por acción. No escribe stdin ni termina procesos directamente.
La cola y los recibos pertenecen al actor nativo y permanecen disponibles al
reabrir sin manager. Cancelar una solicitud Requested la retira atómicamente;
reconocer un Started/EffectUncertain requiere inspección, nota, revisión y lease.
Los estados en curso no ofrecen reconocimiento de recuperación mientras el actor
sigue ejecutando. Ver [native-command-operations.md](native-command-operations.md)
y [persistent-commands.md](persistent-commands.md) para pruebas y límites.

## Apariencia: temas y tipografía

La biblioteca de temas locales usa los controles existentes de limn, incluido ColorPicker. Guardar aplica el tema; cancelar conserva la biblioteca anterior. Se importan YAML y colores JSON de VS Code y se exporta YAML. Los grupos con colores compartidos se identifican en el editor.

El suavizado modifica la colocación subpíxel del renderer; conserva el antialiasing y el hinting.

El alcance y las diferencias frente a Cursor están en [el relevamiento de Apariencia](research/cursor-settings/README.md).

Tema usa un selector compacto con los temas integrados y los temas locales en una
sola lista. La biblioteca conserva las acciones de edición e intercambio; Importar
y Exportar tienen etiquetas sin puntos suspensivos. Los tamaños usan NumberInput
de limn: un solo campo editable entre decremento e incremento, valor centrado, límites y
restablecimiento. Los selectores de fuentes permiten buscar las familias que
descubre el sistema; código ofrece las monoespaciadas para conservar las columnas
del editor y el terminal. Una familia guardada ausente conserva su nombre y señala
su indisponibilidad, mientras el renderer utiliza su fuente de respaldo.

Tipografía incluye una muestra CodeDiff nativa de tres líneas. Usa la familia,
tamaño independiente, sintaxis y fondos de diferencias efectivos del editor. La
búsqueda dentro del selector usa el nuevo slot opcional Select::header de limn,
compartiendo el foco, el scroll de opciones y Escape del menú existente. El detalle
observado con REA está en [tipografía y colores de Cursor](research/cursor-settings/typography-colors.md).


Los menús de Settings usan una sola regla de ámbito al abrir y al terminar de
procesar una acción. Así Apariencia y las opciones de imagen conservan el menú
abierto hasta elegir o descartarlo. La regresión recorre puntero, dispatcher,
reconstrucción y Escape, además de verificar el dibujo del Select.

La explicación permanente de aislamiento usa una fila de la tarjeta. «Permitir
todo» sigue siendo un preset que exige ejecución sin aislamiento; al elegir
«Proyecto, sin red» se revoca. Aprobación y aislamiento son comprobaciones
independientes del harness, pero no se ofrece una combinación contradictoria con
ese preset. El diseño de un permiso sin aprobación limitado al sandbox requeriría
una preferencia y un contrato propios.
