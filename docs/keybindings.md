# Perfiles de teclado y edición Vim

En **Configuración → Atajos** se elige el perfil de navegación y paneles. La selección se guarda para las ventanas IDE y Agents. La tabla de esa pantalla y las ayudas de los controles muestran las combinaciones del perfil activo; los modificadores se adaptan a la plataforma.

- **VS Code** es el perfil predeterminado: `Ctrl+P` abre archivos, `Ctrl+Shift+P` abre la paleta, `Ctrl+B` alterna la barra lateral y `Ctrl+J` alterna el panel inferior. `Ctrl+N` crea un archivo y `Ctrl+Alt+N` crea una tarea.
- **Zed** usa las combinaciones de navegación y paneles adoptadas de su mapa base. Conserva las acciones de OpenIDE; las acciones sin combinación de ese perfil siguen disponibles en sus controles y menús.
- **OpenIDE clásico** conserva `Ctrl+K` para la paleta y `Ctrl+N` para una tarea. La configuración enumera el resto de sus combinaciones activas.

`Ctrl+Alt+E` maximiza o restaura el panel derecho de la ventana Agents en los tres perfiles, como en la ventana de agentes de VS Code. Funciona con el foco en cualquier parte, también en el composer; no actúa con el panel oculto o sin pestañas, ni en la ventana IDE. Escape no lo restaura. El detalle está en [Panel derecho](right-dock.md#maximizar-el-panel).

Estos perfiles cubren las acciones registradas por OpenIDE. No importan automáticamente todos los comandos, extensiones ni archivos de configuración del editor de referencia. El registro central está en `apps/agents-window/src/keymap.rs`; cada acción suministra tanto su combinación funcional como su etiqueta visible.

## Revisión de cambios del agente

Con el foco dentro del editor del archivo que muestra cambios pendientes:

- `Ctrl+Enter` acepta los cambios pendientes **del archivo actual**.
- `Ctrl+Shift+Backspace` rechaza los cambios pendientes **del archivo actual**.
- `F7` va al siguiente bloque; `Shift+F7`, al anterior. En los extremos se conserva el bloque actual.

En macOS se usa `Cmd` en las dos decisiones. Estas combinaciones son contextuales y comunes a los tres perfiles; no actúan desde el composer, la terminal, el buscador del editor, la paleta, otros archivos sin revisión ni un diálogo o menú abierto. Las decisiones también funcionan en la captura confirmada de un archivo eliminado: su cuerpo recibe foco al hacer clic o mediante Tab y sigue siendo de solo lectura. Esta capacidad de foco es optativa de `CodeDiff`; los diffs insertados en el chat conservan su navegación previa. La navegación de bloques requiere la revisión editable con sus posiciones exactas.

Se reutilizan los controles y validaciones de la revisión: los cambios locales sin guardar, una ejecución activa, una versión obsoleta o una operación pendiente impiden aceptar/rechazar. Un acorde reclamado consume también su repetición y liberación, para que no se convierta en una edición o envío si cambia el foco. `Ctrl+Backspace` conserva su función de edición de texto; `Ctrl+Shift+Enter` no acepta cambios.

La referencia de aceptación es [Cursor Keyboard Shortcuts](https://docs.cursor.com/advanced/keyboard-shortcuts). Esa página antigua todavía enumera rechazo sin Shift; se adopta el cambio explícito de 0.48.5 en el [changelog oficial de Cursor](https://cursor.com/changelog/0-48-x), que añade Shift. No se afirma equivalencia completa con todas las versiones actuales de Cursor. `F7`/`Shift+F7` adaptan la convención del visor de diferencias accesible de Code OSS: `src/vs/editor/browser/widget/diffEditor/commands.ts` (`AccessibleDiffViewerNext`/`AccessibleDiffViewerPrev`) en la referencia local de VS Code. No son atajos específicos de Cursor.

`editor_review_app_tests.rs` cubre decisiones durables sólo sobre el archivo enfocado, restauración de un archivo eliminado, modificadores exactos, prioridad de foco y overlays, repetición tras cambiar de foco, navegación y cierre del menú de revisión.

## Vim independiente del perfil

El interruptor **Modo Vim** es independiente del perfil anterior y está desactivado inicialmente. Activarlo conserva el perfil de navegación elegido y habilita el motor modal propio en los editores de archivos. Las vistas de código del chat y las comparaciones de solo lectura no lo habilitan automáticamente.

El editor muestra `NORMAL`, `INSERT`, `VISUAL` o `VISUAL LINE` en una insignia pequeña sin añadir una fila al contenido. Normal y Visual muestran un cursor de bloque; Insert muestra una barra y admite composición IME. El modo se conserva al reconstruir la interfaz del mismo documento; cambiar de documento o desactivar y volver a activar Vim lo reinicia en Normal.

Comandos implementados:

- Movimiento: `h`, `j`, `k`, `l`; `w`, `b`, `e`; `0`, `^`, `$`; `gg` y `G`. Las flechas, Inicio y Fin también funcionan en Normal. Un prefijo numérico repite el movimiento; `3G` o `3gg` van a la tercera línea. Los movimientos verticales conservan la columna visual deseada cuando atraviesan líneas más cortas.
- Inserción: `i`, `a`, `I`, `A`; `o` y `O` abren una línea conservando la indentación. `Esc` termina la inserción y coloca el bloque sobre el grafema anterior de esa línea.
- Selección: `v` activa selección inclusiva por caracteres y `V` selección por líneas. Los movimientos amplían la selección; `Esc` la cancela. La selección por ratón también se sincroniza con el modo visual.
- Operadores: `d`, `c`, `y` seguidos de los movimientos anteriores; `dd`, `cc`, `yy` operan por líneas. Los prefijos de operador y movimiento se multiplican, por ejemplo `2d3w`. `x` y `X` borran grafemas hacia adelante o atrás; `D` y `C` operan hasta el final de la línea. En Visual, `d`, `c`, `y` y `x` actúan sobre la selección.
- Registro sin nombre: `y` y las operaciones de borrado guardan texto; `p` y `P` lo pegan después o antes. Las operaciones de copia también notifican al portapapeles del host. Pegar texto por líneas conserva sus separadores LF/CRLF; pegar un registro de caracteres sobre Visual Line conserva el límite con la siguiente línea.
- Historial: `u` deshace y `Ctrl+R` rehace. Un cambio `c` o una línea abierta con `o`/`O`, seguido de escritura hasta `Esc`, se trata como un gesto de inserción. La agrupación respeta los límites del historial e invalida sus referencias ante ediciones externas.
- Navegación de vista: `Ctrl+B`/`Ctrl+F` desplazan el cursor una página; `Ctrl+U`/`Ctrl+D`, media página; `Ctrl+J`/`Ctrl+N` bajan una línea y `Ctrl+P` sube una línea. En Normal/Visual, estas combinaciones pertenecen al editor enfocado antes que a los paneles. Las ventanas emergentes activas conservan su prioridad de teclado.

El motor trabaja sobre límites de grafemas: un acento combinado o un emoji compuesto no se divide al moverse, borrar o seleccionar. Los conteos se limitan a 1.000; las búsquedas de palabras y el tamaño del registro también están acotados para evitar bloquear la interfaz. Un editor de solo lectura permite navegar, seleccionar y copiar, pero no modificar el texto.

## Alcance actual

Esta implementación es nativa de `openide-ui`; no ejecuta Vim ni Neovim. No incluye comandos Ex (`:`), macros, registros nombrados, marcas, text objects (`iw`, `a{`), búsqueda Vim (`/`, `?`), repetición con `.`, Visual Block ni plugins. La búsqueda de OpenIDE sigue disponible mediante sus controles habituales. Los atajos de Insert permanecen en la capa normal de edición; no se anuncian como una emulación completa de los atajos de inserción de Vim.

La preferencia `vim_mode` y el identificador `keymap_profile` se guardan por separado. Los valores ausentes o inválidos conservan sus valores predeterminados sin descartar otras preferencias.

## Referencias y validación

- Zed separa el mapa base (`references/zed/crates/settings/src/base_keymap_setting.rs`) de su estado Vim (`references/zed/crates/vim/src/vim.rs`). Su selección de cursor por modo y su manejo de pegado en Visual Line (`references/zed/crates/vim/src/normal/paste.rs`) guiaron las decisiones del editor propio.
- Code OSS registra acciones y combinaciones mediante `src/vs/platform/keybinding/common/keybindingsRegistry.ts` y `src/vs/workbench/browser/actions/quickAccessActions.ts`, en la referencia local `/home/nihuel/projects/personal/refs/vscode/`. OpenIDE mantiene una fuente común para despacho y ayudas visuales.
- `crates/openide-ui/src/code_editor_vim_tests.rs` cubre eventos nativos Key+Text, Unicode, estados persistentes, conteos, operadores, lectura sola, historial con expulsión, cambios externos, selección, pegado y pintura. Las comprobaciones de escritorio y sus comandos están en [keymap-desktop-validation.md](keymap-desktop-validation.md).
