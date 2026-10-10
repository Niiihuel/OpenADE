# Entorno de conversación

## Uso

El composer muestra una franja integrada sobre el campo de escritura, en el
orden **Proyecto → Local → Rama**, con Contexto a la derecha. Local permite
seleccionar o crear un worktree. La misma distribución se usa en Nueva tarea
y en las conversaciones. En el chat
del IDE, proyecto y rama conservan sus accesos en el marco del editor; la
franja muestra entorno y contexto. Los permisos están dentro del composer,
junto a adjuntar; modelo, esfuerzo y envío quedan en esa misma fila de acciones.
Los controles mantienen los tokens compactos de OpenIDE. En espacios pequeños,
las etiquetas se reducen y el medidor de contexto conserva su círculo y tooltip.

Los menús de la franja se anclan sobre su control y comparten búsqueda,
selección y acciones con Entorno. Sólo uno puede estar abierto a la vez,
incluidos modelo, permisos y contexto. Local actualiza las carpetas de trabajo
Git al abrirse, incluso con Entorno cerrado. Elegir **Crear rama** o **Crear
worktree** abre el formulario existente de Entorno. Las opciones de worktree
pertenecen a una revisión de Git; una selección anterior a una actualización
no puede cambiar de carpeta.

El icono Entorno está en la cabecera del chat, junto al botón del panel
derecho. La tarjeta agrupa la identidad del proyecto (nombre y ruta), el
selector Local y la rama actual. Control de versiones reúne Cambios del
repositorio, sus sumas `+ / −`, Commit / push y Comparar. **Acciones del
proyecto** tiene un acceso propio; **Fuentes y herramientas** despliega
cambios del chat, archivos consultados, referencias y utilidades.
Las filas usan iconos de 16 px, controles de 24 px y acciones de 28 px, con
hover redondeado y las mismas superficies de OpenIDE.

Entorno y el panel derecho son excluyentes: abrir Entorno colapsa el panel,
y abrir una herramienta o desplegar el panel cierra Entorno. Colapsar conserva
pestañas, borradores, terminales y navegador. Una actualización de pestañas
ocultas no vuelve a desplegar el panel ni cierra Entorno. La terminal inferior
mantiene su visibilidad independiente.

Entorno reserva una columna junto al chat; el historial y el composer se
centran en el ancho restante. La cabecera conserva el ancho completo. Abrir y
cerrar anima el espacio reservado; reducir movimiento aplica el estado final
sin transición. En ventanas estrechas, la tarjeta pasa arriba del chat con
altura acotada y scroll propio para mantener accesible el composer. Los
selectores se despliegan dentro de la tarjeta sin cubrir el historial.
La tarjeta se cierra al cambiar de conversación, carpeta o modo de ventana;
Entorno no ocupa una pestaña del panel derecho.

**Local** muestra las carpetas de trabajo Git conocidas, permite elegir otro
proyecto, copiar su ruta, explorar archivos y crear una rama o un worktree.
La búsqueda de proyectos filtra nombre y ruta; elegir otra carpeta utiliza
el selector nativo existente. El selector de rama conserva el mecanismo
existente de cambio de rama y sus bloqueos durante una ejecución. Las
operaciones de creación parten del commit actual; un worktree requiere una
rama nueva y una ruta absoluta nueva. No se ofrecen entornos remotos sin un
runtime que los implemente.

**Commit o push** muestra los archivos preparados, sin preparar, nuevos y en
conflicto. El acceso **Revisar y preparar archivos** abre Repositorio para
revisar y preparar los cambios. Crear commit exige archivos ya preparados y
un mensaje; no prepara archivos automáticamente. Publicar exige un remoto
conocido y una rama de destino explícita. El remoto de seguimiento o el único
remoto disponible se puede preseleccionar; varios remotos sin seguimiento
requieren selección. El push es normal, sin force. Los conflictos y las
operaciones en curso deshabilitan las acciones que modificarían el repositorio.

**Comparar rama** permite elegir una rama local o remota como base. Muestra
archivos, líneas agregadas y eliminadas, y commits propios y de la base. La
comparación corresponde a commits confirmados desde la base común; los
cambios pendientes siguen en Repositorio. Cuando existe una URL de comparación
GitHub válida, ofrece abrirla en el navegador; no crea un pull request.

Los formularios consultan Git mediante un worker. Cada respuesta pertenece a
una carpeta y revisión, y las operaciones validan el snapshot antes de
modificar refs, index o publicar. Cambiar de carpeta descarta resultados
anteriores. Los errores se presentan en la tarjeta y la actualización manual
permite obtener un nuevo estado.

Las **acciones del proyecto** se guardan por carpeta y tienen nombre y comando
editables. Las sugerencias detectadas en los manifiestos se agregan primero a
la lista; no se ejecutan al abrir Entorno. El botón de ejecutar una acción abre
una terminal propia en esa carpeta y muestra allí su salida. Crear, editar y
eliminar una acción no ejecuta su comando.

La cabecera del chat muestra Entorno, el control del panel derecho y
Configuración con tooltips. El `+` de la barra de pestañas del panel derecho
abre herramientas. El envío conserva la visibilidad elegida y la transfiere
al convertir un borrador en conversación; una conversación sin preferencia
previa empieza con el panel cerrado. Abrir una pestaña es una acción explícita.

Cambios del chat y Cambios del repositorio conservan sus contadores propios;
la cobertura parcial queda visible y un valor desconocido dice **Dato no
disponible**. Los estados de Terminal y Navegador sólo se presentan cuando
pertenecen a una pestaña de la carpeta y conversación actuales. Para otras
pestañas se informa cuántas están abiertas, sin inferir que su proceso esté
listo. Detalles también ofrece acceso a Actividad.

Fuentes incluye los adjuntos del borrador, imágenes enviadas, archivos
consultados mediante `read_file` completado sin error y referencias web
registradas. Los archivos se abren tras validar que su ruta pertenece a la
carpeta actual. Las referencias HTTP/HTTPS abren el navegador del sistema.
Las imágenes enviadas muestran una miniatura y datos registrados; todavía
no tienen una galería propia. Cada grupo resume hasta 32 entradas y avisa
cuántas quedaron fuera. La tarjeta tiene su propio scroll; las listas largas
de opciones reservan un viewport acotado y las listas cortas usan su altura
natural.

Contexto se mantiene en la franja superior del composer y, para una cuenta
Codex conectada, Uso del plan conserva su acceso independiente. Son información
del contexto y los límites de la cuenta, respectivamente; no abren Entorno.
El acceso Conectar proveedor aparece
únicamente en la pantalla de modelos del selector.

Todas las vistas y controles están implementados con `openide-ui`. El host
Rust mantiene las pestañas por carpeta y conversación. Ocultar el dock o
cambiar de tarea conserva sus terminales y navegadores; cerrar la pestaña o
eliminar su conversación libera esos recursos.

## Cambios del chat

`session_changes.rs` toma un baseline privado antes de los efectos del primer
turno Build, después de la validación del runner. Se guarda en
`sessions/session-changes/<session>.json`, con escritura atómica y checksum.
El baseline captura hasta 8 MiB de contenido y 512 KiB por archivo; los
enlaces y archivos omitidos no se consideran contenido conocido.

El worker reconstruye las escrituras y patches confirmados en el journal.
Genera los diffs entre ese baseline y los contenidos reconstruidos, usando
temporales privados y `git diff --no-index`. No cambia index, refs ni archivos
del proyecto. La lista virtualizada conserva la selección por ruta y presenta el diff del
archivo elegido. Las respuestas de otra carpeta, conversación o generación
se descartan.

La cobertura puede ser completa, parcial o no disponible. Comandos, hooks,
procesos, MCP y subagentes no aportan snapshots de sus efectos en cada archivo;
las operaciones manuales tienen sidecars separados. El inicio de servidores
MCP externos se registra antes de arrancarlos, porque su preparación y cierre
también pueden afectar el proyecto. Después de esos efectos,
un patch que depende de contenido previo desconocido no se reconstruye.
Las escrituras completas confirmadas pueden mostrarse como cambios históricos
con cobertura parcial. Los chats anteriores sin baseline dicen **Dato no
disponible**. Cambios del repositorio permite consultar el estado Git actual.

El registro describe efectos confirmados de esta conversación. No representa
un snapshot atómico de todo el disco ni detecta ediciones externas concurrentes.
La activación interna de instrucciones de proyecto se reconoce por su pareja
de eventos y resultado autenticado, y no se confunde con un hook que ejecuta
comandos. Los marcadores incompletos o ambiguos mantienen cobertura parcial.

## Navegador

`browser_runtime.rs` supervisa un proceso Node con la biblioteca Playwright.
Chromium usa un contexto aislado; la vista nativa dibuja sus capturas PNG y
envía clics, texto, teclas y scroll al mismo actor que atiende las herramientas
del agente. Hay navegación atrás/adelante, recarga, dirección y actualización
de captura. La implementación utiliza las APIs públicas de
[Playwright Library](https://playwright.dev/docs/library) y
[Page](https://playwright.dev/docs/api/class-page).

Abrir el panel no descarga dependencias. Si faltan, **Instalar navegador**
instala Playwright 1.63.0 y Chromium en el directorio administrado de datos;
requiere Node, npm y las dependencias del sistema de Chromium. Hay un máximo de
cuatro runtimes, cola compartida de 16 solicitudes, acciones con deadline y
límites de JSON, capturas y árbol accesible. La vista se actualiza periódicamente
cuando está visible; no es un WebView ni un video continuo. Esta primera versión
mantiene una página, cierra ventanas emergentes y no acepta descargas.
Los temporales del navegador son propios y sobreviven al cierre del shell que
inició la app. En NixOS se puede usar el Chromium o Chrome empaquetado del
sistema, siempre con un perfil aislado y sandbox habilitado. La selección del
motor, la configuración y las pruebas reales se describen en
[Runtime del navegador](browser-runtime.md).

En el próximo turno Build, un navegador abierto y listo de la conversación
se anuncia como `mcp__openide_browser__…`. El agente recibe estado, URL y árbol
accesible; el adapter nativo actual devuelve contenido textual de MCP. El
proxy externo también ofrece captura PNG. En el motor de OpenIDE, los argumentos
pasan por el catálogo validado y la política de permisos existente; un cliente
MCP externo administra sus propios permisos. Cancelar una acción ya despachada
no provoca un reintento automático. La identidad del runtime interviene en la
validación de recuperación de herramientas pendientes.

El botón de conexión MCP copia configuración JSON de un proxy stdio conectado
al mismo actor. Su endpoint y capacidad son efímeros: sirven mientras la
pestaña permanezca abierta. Puede adaptarse al formato de configuración del
cliente externo; no modifica la configuración personal de Codex.

La [documentación oficial de Codex Browser](https://learn.chatgpt.com/docs/browser)
sitúa su navegador integrado en la aplicación de escritorio. OpenIDE proporciona
su propia conexión, siguiendo el patrón de runtime controlado por herramientas
descrito en la [guía oficial de Computer Use](https://developers.openai.com/api/docs/guides/tools-computer-use).

## Referencias

ZCode, revisión `29628c9acdb81b703bbd4080c207a0e7ce5e276e`:

- [Selector de proyecto](https://github.com/zai-org/ZCode/blob/29628c9acdb81b703bbd4080c207a0e7ce5e276e/packages/ui/src/ChatEmptyState.tsx).
- [Identidad del dock](https://github.com/zai-org/ZCode/blob/29628c9acdb81b703bbd4080c207a0e7ce5e276e/packages/ui/src/lib/workspaceSidePane.ts).
- [Panel Git](https://github.com/zai-org/ZCode/blob/29628c9acdb81b703bbd4080c207a0e7ce5e276e/packages/ui/src/v4/ConversationStatusPanel.tsx).
- [Runtime de navegador](https://github.com/zai-org/ZCode/blob/29628c9acdb81b703bbd4080c207a0e7ce5e276e/packages/ui/src/browser-use/UnifiedBrowserView.tsx).

OpenChamber, revisión `3c0d1e3b4384f0da612f504a820f1ddec2c5da79`,
clonada en `references/openchamber` sin instalar ni ejecutar sus paquetes:

- [Panel de utilidades](https://github.com/openchamber/openchamber/blob/3c0d1e3b4384f0da612f504a820f1ddec2c5da79/packages/ui/src/components/layout/ContextPanel.tsx): separa Git, archivos, diffs, navegador y terminal; conserva las páginas de navegador montadas cuando quedan ocultas y mantiene el PTY por directorio.
- [Identidad de contexto del proyecto](https://github.com/openchamber/openchamber/blob/3c0d1e3b4384f0da612f504a820f1ddec2c5da79/packages/ui/src/components/layout/RightSidebarTabs.tsx): deriva el proyecto del directorio de la conversación y evita tomar datos de otra carpeta.

La tarjeta compacta y los flujos de Git toman como referencia el entorno local
de Codex: el documento oficial muestra Cambios, Local, rama y Commit o push,
y describe preparar archivos antes de confirmar y publicar. La revisión de
código distingue cambios sin preparar, preparados y comparación de ramas.
OpenIDE conserva sus componentes `openide-ui`, worker Git y registro de
cambios propios; la comparación web abre GitHub y no crea un pull request.

- [Codex: entorno local](https://learn.chatgpt.com/docs/environments/local-environment).
- [Codex: revisión de código](https://learn.chatgpt.com/docs/code-review).
- [Codex: worktrees de Git](https://learn.chatgpt.com/docs/environments/git-worktrees).
- [Claude Code Desktop: entornos y sesiones](https://code.claude.com/docs/en/desktop): diferencia entorno, carpeta, modelo y permisos, y mantiene terminales y archivos como utilidades del proyecto. OpenIDE ofrece los entornos locales y worktrees que su runtime implementa.
- [codex-docs.com](https://www.codex-docs.com/docs): sitio de terceros consultado a pedido del usuario; no es documentación oficial de OpenAI.

La organización detallada también toma las filas de Git de ZCode y la
separación de utilidades de OpenChamber. No incorpora sus stacks React/Electron.

El `+` sigue la barra de pestañas de `AnimatedSidePanePanel.tsx` de ZCode y
el flujo **New tab → Terminal** de la
[documentación oficial de Codex](https://learn.chatgpt.com/docs/integrated-terminal).

El diff del último turno de `useGitRepository.ts` aún tiene entradas nulas en
esa revisión. OpenIDE usa su journal y baseline propios. La implementación
del host, componentes y bridge es propia; no se copiaron fuentes de Zed.
