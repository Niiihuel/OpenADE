# Perfiles de atajos: validación nativa

`scripts/keymap-desktop-e2e.py` ejecuta una copia con `editor-e2e` en un
compositor Wayland privado. El proyecto, las preferencias y las terminales
son temporales. Los atajos se envían mediante teclado nativo; el observador
sólo informa el estado que produjo la aplicación.

## Casos

- Una instalación vacía empieza con el perfil VS Code. `Ctrl+Shift+M` abre
  Problemas; `Ctrl+J` oculta y recupera Problemas, Salida y Consola sin crear
  una sesión de terminal. En Agents se comprueban por separado `Ctrl+P`
  para Archivos y `Ctrl+Shift+P` para Acciones.
- Con código seleccionado, `Ctrl+X` corta y Deshacer lo restaura.
  `Ctrl+Shift+X` abre Complementos sin cortar ni alterar esa selección,
  incluso cuando el adaptador nativo entrega un evento de portapapeles.
- `Ctrl+J` desde el PTY oculta y recupera su panel. `Ctrl+\`` abre o enfoca
  la terminal; cuando ya tiene el foco, la oculta. Al mantener `Ctrl+J`
  durante 900 ms, el panel se abre una sola vez sin filtrar repetición al PTY.
- `Ctrl+Shift+P` y `F1` abren la paleta en Acciones. `Ctrl+P` abre Archivos
  con el campo enfocado. La escritura pertenece al buscador; los atajos del
  panel no operan detrás de esta ventana.
- El selector de perfiles de terminal y el diálogo Nuevo archivo conservan
  la prioridad del teclado; Escape cierra la superficie correspondiente.
- Un proceso temporal pone el PTY en modo raw y registra cada byte recibido.
  Los atajos consumidos por la aplicación deben producir **cero bytes** en
  ese registro. Su salida explícita usa una tecla separada y comprueba que
  el shell original conserve su PID.
- Configuración → Atajos permite elegir Zed y OpenIDE clásico. La elección
  se verifica en la interfaz y en el archivo persistido, y después de cerrar
  y volver a iniciar el proceso sobre ese mismo estado temporal.
- OpenIDE clásico conserva `Ctrl+K` con el alcance Todos. Elegir VS Code
  vuelve a activar su paleta con `Ctrl+Shift+P`.
- Vim se activa por separado del perfil de navegación. Se comprueban la
  entrada Normal/Insert, movimientos, borrado Unicode, deshacer, selección
  visual y persistencia. `Ctrl+J` pertenece al editor Vim cuando éste tiene
  el foco y vuelve a controlar el panel inferior fuera del editor.

Esta prueba no conecta modelos: el selector de modelo/esfuerzo del perfil
clásico requiere opciones de modelos disponibles. Su resolución de atajo se
valida mediante los tests de registro y despacho del host. El perfil de
teclado y el modo de edición Vim son preferencias independientes.

## Ejecución

```sh
rtk python3 scripts/headless-desktop-fixture.py \
  --kwin /ruta/a/kwin_wayland --output /tmp/openide-keymap-private \
  -- python3 scripts/keymap-desktop-e2e.py \
  --binary /tmp/openide-keymap-e2e-bin --wtype /ruta/a/wtype \
  --output /tmp/openide-keymap-native
```

El informe final sólo se escribe cuando toda la suite pasa. Ante un error se
conservan `failure.json`, la última captura y el registro de bytes del PTY.
La producción se compila sin `editor-e2e`. Ningún evento se dirige al
escritorio real del usuario.

## Revalidación de teclas y paleta · 7 de octubre de 2026

La revisión de `Kbd` a 20 px de alto y radio de 4 px pasó nuevamente los
**9/9 grupos** sobre `/tmp/openide-kbd-e2e-bin`, con salida 0. El informe está
en `/tmp/openide-kbd-keymap-native/report.json`, junto a 16 capturas nativas.
El fixture aislado fue `/tmp/openide-keymap-e2e-vzhykxto`.
`native-keymap.log` y el registro raw del PTY quedaron vacíos.

Se inspeccionaron las paletas Acciones, Archivos y Todo, y la página Atajos.
El campo ocupa el ancho de la cabecera; las categorías forman un control
segmentado alineado al inicio, con cada etiqueta centrada en su segmento.
Las flechas vectoriales, Enter/Abrir y Esc/Cerrar se leen por separado en el
pie, sin solaparse con el campo. La combinación larga de duplicar se conserva
completa en Ajustes. Estas capturas comprueban la presentación nativa; las
pruebas de geometría de la paleta se ejecutan en la suite del host.

La misma compilación aprobó los **17/17 casos** direccionales y de keycaps
descritos en [Validación de tooltips](tooltip-desktop-validation.md).

## Resultado anterior del 7 de octubre de 2026

La suite completó **9/9 grupos**, con código de salida 0, sobre
`/tmp/openide-keymap-e2e-bin`. Se conservaron 16 capturas nativas y el
informe `/tmp/openide-keymap-native/report.json`. El registro de la aplicación
`native-keymap.log` quedó vacío. El estado temporal reside en
`/tmp/openide-keymap-e2e-n_cq3zae`.

El registro raw del PTY quedó en **0 bytes**, incluido `Ctrl+J` sostenido
900 ms. Se comprobó que el shell mantuvo el mismo PID, que alternar las
otras tres secciones no creó terminales y que las preferencias sobrevivieron
al reinicio del proceso. El código del proyecto temporal quedó intacto.

La prueba encontró una regresión real: al abrir la paleta desde el PTY,
Escape conservaba el foco base de la terminal e impedía cerrar la paleta.
El host se corrigió y el pase final verifica que Escape cierre la superficie
activa sin enviar ese evento al PTY.

Se inspeccionaron las capturas de las paletas, selector de perfiles, ajustes
Vim y modos Normal/Visual. El pase final se repitió después de sustituir la
flecha del atajo de duplicación por `Down`: la keycap se lee completa en
`keymap-vscode-settings-native.png`, sin glifo de sustitución. La fuente del
fixture presenta el emoji de prueba
como glifo de sustitución; la entrada, selección, borrado y deshacer Unicode
se validaron contra el contenido exacto del buffer. La prueba no certifica
la cobertura tipográfica de todos los alfabetos y emojis.

## Resultados complementarios

Una pasada anterior de la verificación centralizada completó 1573 pruebas
del host, 477 de `openide-ui` y 49 del motor `openide-editor`. Es un resultado
histórico de aquella revisión; no describe como verde todo el árbol actual.

La última suite del host, ejecutada sobre la copia estable
`/tmp/openide-keymap-host-tests`, terminó con **1581 aprobadas, 3 fallidas y
67 ignoradas**. Los tres fallos pendientes son:

- `app::chat_diff_app::tests::inline_file_cards_preserve_basename_native_font_budget_and_full_path_tooltip`
- `app::native_subagent_stream_app_tests::child_updates_do_not_inherit_a_foreign_transcript_layout_or_scroll_another_session`
- `session_surface::plan_visual_tests::inline_plan_keeps_chat_viewport_dock_and_unsent_selection`

Corresponden a comprobaciones de presentación de archivos, desplazamiento
entre sesiones y conservación del área de conversación. Hubo cambios
concurrentes en esas superficies durante la validación; esta ejecución no
aísla la causa de cada fallo. No se modificaron esas pruebas para obtener
un resultado favorable.

Las pruebas dirigidas de perfiles de teclado completaron **11/11** y el
último pase de escritorio mantuvo **9/9 grupos aprobados**. La compilación
de producción terminó sin errores, con 26 advertencias preexistentes.
OpenIDE se inició con esa compilación y su registro de arranque quedó
vacío. Estos resultados validan el flujo de perfiles/Vim comprobado aquí;
la suite general del host conserva los tres fallos enumerados.
