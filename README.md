# OpenIDE Agents Window

Una ventana de agentes nativa en Rust, con un framework de UI propio, un harness
propio y adaptadores configurables para agentes de línea de comandos.

El botón **IDE / Agentes** cambia la distribución del mismo espacio de trabajo.
La vista IDE reúne explorador, editor con pestañas, terminal inferior y agente
lateral, conservando los documentos y la sesión. Usa los componentes nativos de
`openide-ui`; los detalles están en [Espacio de trabajo IDE](docs/ide-workspace.md).

Markdown ofrece Código, Vista previa y vista dividida sobre el mismo buffer.
Imágenes y PDF se abren en visores nativos con zoom y ajuste al panel. El motor
Rust compartido se documenta en [Vistas previas de documentos](docs/native-document-preview.md).

Los [plugins locales](docs/plugins.md) pueden aportar herramientas MCP al agente.
Se revisan antes de instalarse y se activan por proyecto desde Configuración → Plugins.
El repositorio incluye un [ejemplo mínimo](examples/plugins/project-info/README.md).

## Estado

El proyecto está en desarrollo. Las implementaciones originales viven en
`crates/` y la aplicación de escritorio en `apps/agents-window/`. Los clones de
Zed, ZCode y Codex están en `references/` para estudiar su arquitectura; esa carpeta
queda fuera del proyecto y no se incorpora a sus binarios.

## Desarrollo

Requiere Rust estable y las herramientas de compilación nativas de la plataforma.

```sh
cargo test --workspace
cargo run --profile desktop -p agents-window
```

En Linux, para compilar reutilizando la caché incremental y limpiar automáticamente
los artefactos de builds anteriores:

```sh
python3 scripts/build.py
python3 scripts/build.py --run
```

En NixOS, si el compilador C o `pkg-config` no están en el entorno:

```sh
nix-shell -p stdenv.cc pkg-config wayland libxkbcommon --run 'python3 scripts/build.py'
```

El script utiliza por defecto el perfil optimizado `desktop`, con ejecutable en
`target/desktop/agents-window`. Evita ejecutar la interfaz sin optimización: el
coste de medir texto y dibujar cambia mucho en el perfil `dev`. Para depurar con
compilaciones rápidas se puede usar `python3 scripts/build.py --profile dev --run`.

El comando usa siempre `target/`, conserva el ejecutable actual y las dependencias
de la última compilación correcta, y elimina los artefactos con hashes antiguos de
`target/<perfil>/deps/`, sin tocar los otros perfiles. Si la compilación falla,
no ejecuta la limpieza. La caché
incremental y los archivos auxiliares de Cargo se conservan para evitar recompilar
todo en cada cambio. Los comandos directos de Cargo siguen funcionando, pero la
limpieza automática se ejecuta mediante este script.

El perfil de desarrollo conserva información de líneas para los backtraces y
omite los símbolos de depuración de las dependencias. Para una sesión de depuración
con inspección de variables se puede usar `CARGO_PROFILE_DEV_DEBUG=2`, a costa de
más espacio y una recompilación. La caché no es un historial de versiones ni tiene
un límite fijo de tamaño: puede crecer con las dependencias y los tests.

En el entorno Windows actual, el toolchain GNU puede compilar sin instalar
Visual Studio Build Tools:

```powershell
$env:PATH = "$env:USERPROFILE\.local\toolchains\msys2-mingw64-binutils\mingw64\bin;$env:PATH"
& "$env:USERPROFILE\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu test --workspace
& "$env:USERPROFILE\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu run -p agents-window
```

Para revisar la UI sin leer las tareas ni automatizaciones del perfil habitual,
define `OPENIDE_DATA_ROOT` con una ruta absoluta a una carpeta de datos aislada
antes de ejecutar la ventana.

En Configuración → Proveedores y modelos, OpenIDE mantiene un catálogo propio
alimentado directamente por las APIs oficiales de los proveedores registrados,
con caché local de la última respuesta válida. El registro incorporado incluye
OpenAI (Responses), OpenRouter, DeepSeek, Groq, Mistral y xAI (Chat Completions),
y permite agregar proveedores con esos protocolos mediante
`provider-registry.json` en el directorio de datos. Cada adaptador declara su
endpoint, variable de clave y rutas de metadatos; la respuesta del catálogo no
puede cambiarlos. La conexión personalizada
usa `OPENIDE_API_KEY`, `OPENIDE_API_BASE` y `OPENIDE_MODEL` como valores iniciales
para instalaciones anteriores; puede funcionar sin clave con un servidor local.
La selección se guarda en la carpeta de datos local, sin guardar la clave. Los
perfiles CLI usan la autenticación de cada programa instalado. `.env.example`
muestra los nombres de las claves incorporadas, pero la aplicación no carga
archivos `.env` automáticamente. Los esfuerzos se ofrecen sólo cuando el
modelo exacto los declara o una revisión específica los confirma y el
adaptador puede enviarlos por su protocolo. Los modelos sin esos datos quedan
en «Automático». Una selección explícita sin verificación conserva límites
desconocidos, sin compactación automática basada en una ventana inventada.
El selector del composer agrupa las rutas por proveedor. Consulta en segundo
plano el listado autenticado declarado por cada adaptador y conserva las
respuestas sólo en memoria. El
listado autenticado, cuando responde, acota los modelos visibles de esa cuenta.
Por separado, el catálogo
normalizado conserva localmente su última lista válida para consulta sin
conexión, sin convertirla en prueba de disponibilidad actual. El menú muestra
una acción de conexión si falta la clave. Codex CLI es una ruta separada: OpenIDE consulta
`codex app-server` para mostrar los modelos y esfuerzos anunciados por la
sesión ChatGPT activa, sin leer ni transferir sus credenciales. Si esa consulta
falla, el menú identifica un catálogo de respaldo y Codex valida la selección
al ejecutar. Los niveles de esfuerzo provienen del par exacto
proveedor/modelo; el slider ajusta al nivel más cercano y ofrece
«Automático» aparte. La interfaz no guarda API keys y hoy las recibe mediante
variables de entorno, tal como se indica arriba.
La [arquitectura de proveedores y catálogo propio](docs/providers-models-architecture.md)
documenta la arquitectura Rust, el alcance actual y los pasos pendientes.

La aplicación permite elegir un backend instalado y un directorio de trabajo.
La barra lateral muestra el harness nativo y sólo los perfiles CLI cuyo
ejecutable se encuentra en `PATH` al iniciar la ventana. En este equipo se
detectó Codex; los demás aparecerán al instalarlos y reiniciar OpenIDE. Los
perfiles personalizados se definen en `cli-profiles.json`.
La elección explícita de backend se conserva al reiniciar. Sin elección previa,
el harness nativo es la ruta inicial; los perfiles CLI se usan al elegirlos
expresamente. Las sesiones Codex CLI usan su propio proceso y las sesiones
nativas usan el harness Rust de OpenIDE.
El encabezado de la conversación identifica la ruta usada. Los avisos que
Codex CLI escriba en stderr aparecen como diagnósticos en Actividad; el turno
no se marca como fallido por la sola presencia de stderr.
La conexión nativa a OpenAI usa una clave de API del proyecto, como exige la
[guía oficial de la API](https://developers.openai.com/api/docs/quickstart).
El inicio de sesión de Codex CLI pertenece a ese perfil externo y no configura
automáticamente la conexión nativa.
Las claves de los proveedores se leen de variables de entorno y no se guardan
en el historial de sesiones.

La ventana separa tres superficies redimensionables: sesiones y archivos a la
izquierda, conversación y composer en el centro, y pestañas de contexto,
actividad, cambios de Git y vista previa de texto a la derecha. La vista de
sesión agrupa llamadas de herramientas, muestra sus estados reales y conserva
el composer visible durante el scroll. El texto estático y los párrafos con
formato admiten selección por arrastre y copia con `Ctrl+C`; la vista previa de
archivos muestra números de línea y resaltado discreto. El panel de cambios muestra una lista virtual de archivos y carga el diff al
seleccionar uno. Las acciones explícitas por archivo permiten preparar o quitar
de preparados mediante un worker, conservando el contenido en disco.
Si el proyecto tiene un remoto de `github.com`, el mismo dock consulta los PR
abiertos de la rama actual fuera del hilo visual. Para repositorios privados
puede leer `GH_TOKEN` o `GITHUB_TOKEN` del entorno; consulta
[el alcance de GitHub](docs/github.md).
Las pestañas del dock conservan
su identidad por proyecto y tarea, admiten cierre individual, arrastre y
navegación con `Ctrl+Tab`. El menú `+` crea terminales independientes.
La tarjeta [Entorno](docs/conversation-environment.md) reúne cambios, ramas,
worktrees locales, commit, push y comparación de ramas. Su botón `+` permite
guardar acciones del proyecto y ejecutarlas en terminales independientes.
Al abrirla, el chat y el composer se acomodan en el espacio restante con una
transición; la tarjeta y sus selectores no cubren la conversación.
Consulta [la estructura e interacción del panel derecho](docs/right-dock.md). Las sesiones admiten título, fijado, archivo,
eliminación confirmada y borradores persistentes por tarea, con guardado al
cerrar. Un registro dañado se informa sin ocultar las otras sesiones.
«Eliminar…» está disponible en conversaciones normales y archivadas. La
confirmación elimina su historial y subagentes, conservando los archivos del
proyecto. Las ejecuciones activas y los cambios de rama bloquean la operación.
El selector Grupo/Proyecto permite organizar tareas en grupos manuales o por
carpeta; el filtro conserva la vista cronológica y el orden por fecha. Los
grupos y el estado plegado de la barra se guardan localmente. Los controles de
icono principales muestran una ayuda flotante con puntero.

La pantalla inicial centra el composer. `Ctrl+K` abre el buscador de acciones,
sesiones y archivos del proyecto; `Ctrl+N` crea una tarea. La navegación de
atrás/adelante conserva el contexto de sesión, archivos y configuración.
`Ctrl+B` pliega y reabre la barra lateral.

El harness nativo empieza con herramientas de lectura. En Configuración → Agente
se guardan permisos por herramienta; al activar escritura o comandos, cada
llamada a `write_file` o `run_command` pide permiso
en el composer. La decisión se registra de forma durable antes de enviarse al
runner; un fallo de guardado deniega la llamada. Los agentes CLI instalados
siguen el flujo de permisos de sus propias herramientas.
Para sesiones nativas, el composer separa Plan/Build de los permisos. Plan
anuncia solamente las herramientas locales de lectura y el runner rechaza
las llamadas fuera de ese catálogo; Build aplica los permisos configurados.

La interfaz usa la estructura y densidad de ZCode con una paleta nativa oscura.
El composer tiene un acento azul en Enviar y un gradiente azul-violeta en el
selector de esfuerzo. Los iconos Solar Linear son monocromos, reciben una
sola tinta del control y se alinean a píxeles físicos para verse nítidos. Consulta
[la iconografía](docs/iconography.md) y [los perfiles CLI](docs/cli-profiles.md). El framework Rust implementa
capas flotantes, sombras, estado por ID y transiciones de controles; el host
actual usa egui/wgpu para ventanas, fuentes e input.

La paridad con ZCode y la calidad de Warp siguen en curso: el Browser embebido,
la revisión interactiva de diffs y las vistas de medios necesitan backends
propios. La terminal integrada ya usa PTY.

## Fuentes de referencia y licencias

- [Warp](https://github.com/warpdotdev/warp): `warpui` y `warpui_core` se publican
  bajo MIT; el resto del repositorio se publica bajo AGPL v3. Nuestra UI usa
  conceptos generales (vistas, elementos, estado, comandos de dibujo) con código
  escrito desde cero. No incorpora código AGPL de Warp.
- [ZCode](https://github.com/zai-org/ZCode): se usa como referencia funcional para
  el ciclo de agente, herramientas, sesiones y proveedores. El código de este
  proyecto se escribe en Rust desde cero.
- [Codex](https://github.com/openai/codex): se estudian los contratos de turno,
  herramientas y recuperación de su harness Rust. OpenIDE mantiene su propia
  implementación; el código de Codex se publica bajo Apache-2.0.

Consulta [docs/architecture.md](docs/architecture.md) para el mapa de componentes
y el alcance funcional.
La [decisión y el mapa del harness nativo](docs/native-harness-roadmap.md)
registran las capacidades pendientes y los criterios de verificación.
