# OpenIDE Agents Window

Una ventana de agentes nativa en Rust, con un framework de UI propio, un harness
propio y adaptadores configurables para agentes de línea de comandos.

## Estado

El proyecto está en desarrollo. Las implementaciones originales viven en
`crates/` y la aplicación de escritorio en `apps/agents-window/`. Los clones de
Warp y ZCode están en `references/` para estudiar su arquitectura; esa carpeta
queda fuera del proyecto y no se incorpora a sus binarios.

## Desarrollo

Requiere Rust estable y las herramientas de compilación nativas de la plataforma.

```sh
cargo test --workspace
cargo run -p agents-window
```

En el entorno Windows actual, el toolchain GNU puede compilar sin instalar
Visual Studio Build Tools:

```powershell
$env:PATH = "$env:USERPROFILE\.local\toolchains\msys2-mingw64-binutils\mingw64\bin;$env:PATH"
& "$env:USERPROFILE\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu test --workspace
& "$env:USERPROFILE\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu run -p agents-window
```

En Configuración → Proveedores y modelos, el harness nativo consulta el catálogo
de [Models.dev](https://models.dev/) en segundo plano, conserva la última copia
válida y permite elegir proveedor y modelo. Por ahora los adaptadores de Chat
Completions admiten OpenAI (`OPENAI_API_KEY`), OpenRouter
(`OPENROUTER_API_KEY`) y DeepSeek (`DEEPSEEK_API_KEY`). La conexión personalizada
usa `OPENIDE_API_KEY`, `OPENIDE_API_BASE` y `OPENIDE_MODEL` como valores iniciales
para instalaciones anteriores; puede funcionar sin clave con un servidor local.
La selección se guarda en la carpeta de datos local, sin guardar la clave. Los
perfiles CLI usan la autenticación de cada programa instalado. `.env.example`
muestra los nombres, pero la aplicación no carga archivos `.env`
automáticamente. Un modelo visible en el catálogo sólo se puede seleccionar
para ejecutar si existe un adaptador compatible y admite texto y herramientas.
Los modelos nuevos aparecen como metadata al refrescar el catálogo; la
ejecución de cada protocolo se habilita tras verificar su compatibilidad.
La [investigación de proveedores, Models.dev y OpenCode ACP](docs/providers-models-architecture.md)
documenta la arquitectura Rust, el alcance actual y los pasos pendientes.
La [estructura completa de Configuración](docs/settings-navigation.md) reúne
20 secciones nativas; el [mapa de capacidades](docs/settings-capability-map.md)
indica cuáles tienen servicio Rust conectado y cuáles son informativas.

La aplicación permite elegir un backend instalado y un directorio de trabajo.
La barra lateral muestra el harness nativo y sólo los perfiles CLI cuyo
ejecutable se encuentra en `PATH` al iniciar la ventana. En este equipo se
detectó Codex; los demás aparecerán al instalarlos y reiniciar OpenIDE. Los
perfiles personalizados se definen en `cli-profiles.json`.
Las claves y configuración privadas se leen del entorno local; nunca se guardan
en el historial de sesiones.

La ventana separa tres superficies redimensionables: sesiones y archivos a la
izquierda, conversación y composer en el centro, y pestañas de contexto,
actividad, cambios de Git y vista previa de texto a la derecha. La vista de
sesión agrupa llamadas de herramientas, muestra sus estados reales y conserva
el composer visible durante el scroll. El texto estático y los párrafos con
formato admiten selección por arrastre y copia con `Ctrl+C`; la vista previa de
archivos muestra números de línea y resaltado discreto. El panel de cambios lee los diffs
preparados y sin preparar de forma acotada, sin modificar el repositorio.
Si el proyecto tiene un remoto de `github.com`, el mismo dock consulta los PR
abiertos de la rama actual fuera del hilo visual. Para repositorios privados
puede leer `GH_TOKEN` o `GITHUB_TOKEN` del entorno; consulta
[el alcance de GitHub](docs/github.md).
Las pestañas del dock conservan
su identidad por proyecto y tarea. Las sesiones admiten título, fijado, archivo,
eliminación confirmada y borradores persistentes por tarea, con guardado al
cerrar. Un registro dañado se informa sin ocultar las otras sesiones.
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

La interfaz usa la estructura y densidad de ZCode con una paleta nativa oscura
**neutra**, sin acento azul. Los iconos Solar Linear son monocromos, reciben una
sola tinta del control y se alinean a píxeles físicos para verse nítidos. Consulta
[el mapa visual](docs/zcode-visual-map.md), [la guía de componentes y layout de WarpUI](docs/warp-visual-system.md),
[la iconografía](docs/iconography.md) y [los perfiles CLI](docs/cli-profiles.md). El framework Rust implementa
capas flotantes, sombras, estado por ID y transiciones de controles; el host
actual usa egui/wgpu para ventanas, fuentes e input.

La paridad con ZCode y la calidad de Warp siguen en curso: el Browser embebido,
la revisión interactiva de diffs y las vistas de medios necesitan backends
propios. La terminal integrada ya usa PTY. La [auditoría de WarpUI](docs/warp-gap-analysis.md)
detalla las brechas del framework; el [estudio del panel derecho](docs/zcode-right-dock.md)
detalla su alcance actual.

## Fuentes de referencia y licencias

- [Warp](https://github.com/warpdotdev/warp): `warpui` y `warpui_core` se publican
  bajo MIT; el resto del repositorio se publica bajo AGPL v3. Nuestra UI usa
  conceptos generales (vistas, elementos, estado, comandos de dibujo) con código
  escrito desde cero. No incorpora código AGPL de Warp.
- [ZCode](https://github.com/zai-org/ZCode): se usa como referencia funcional para
  el ciclo de agente, herramientas, sesiones y proveedores. El código de este
  proyecto se escribe en Rust desde cero.

Consulta [docs/architecture.md](docs/architecture.md) para el mapa de componentes
y el alcance funcional.
