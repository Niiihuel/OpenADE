# Menús, atajos y editor vacío: validación nativa

`scripts/menu-desktop-e2e.py` abre un proceso de OpenIDE con la función
`editor-e2e` en un compositor Wayland privado. Crea un proyecto Git de prueba
sin cuentas configuradas. Las acciones se ejecutan con teclado y puntero reales;
el observador solamente publica el estado y la geometría del frame pintado.
No se envían eventos al escritorio del usuario.

## Casos

- «Ver»: pasar el puntero a Buscar deja un único resaltado aunque Archivos y
  los paneles estén marcados. Flecha abajo continúa desde esa fila; Enter
  activa Control de versiones.
- La barra de menús cambia de categoría con el puntero y las flechas. Escape
  y un clic fuera cierran la superficie.
- Guardar está deshabilitado para un documento sin cambios. Nuevo archivo
  abre su diálogo con el campo enfocado; cancelarlo no crea archivos.
- El clic derecho nativo abre el menú del editor con ancho acotado. Escape
  devuelve el foco al documento.
- A 1400 × 360, «Ver» permanece debajo de su disparador y permite llegar a
  Configuración mediante la rueda, sin tapar la barra de menús.
- A 760 × 640, el menú compacto abre submenús con el puntero y el teclado.
  Flecha izquierda vuelve al padre sin reabrir el hijo; Escape cierra el padre.
- Al cerrar el último archivo, el editor muestra la marca de OpenIDE y tres
  acciones a 1400 × 900 y 680 × 640. Los atajos tienen teclas con borde y se
  mantienen dentro de su fila. Los botones abren archivos, comandos y terminal.
- El menú de acciones de la terminal comparte filas compactas y devuelve el
  foco a su disparador al cerrarse.

El script compara las medidas nativas de etiquetas y teclas con sus regiones,
comprueba que los bytes del archivo no cambian y guarda capturas PNG. Un fallo
queda en `failure.json`; `report.json` solamente se escribe cuando pasa toda la
suite. Los submenús conservan su fallo aunque se continúen verificaciones
independientes para obtener más evidencia en el mismo pase.

## Ejecución

Compilar primero una copia del ejecutable con `--features editor-e2e`. Luego:

```sh
rtk python3 scripts/headless-desktop-fixture.py \
  --kwin /ruta/a/kwin_wayland --output /tmp/openide-menu-private \
  -- python3 scripts/menu-desktop-e2e.py \
  --binary /tmp/openide-menu-e2e-bin --wtype /ruta/a/wtype \
  --output /tmp/openide-menu-native
```

El ejecutable de producción se compila sin `editor-e2e`.

## Referencia de interacción

Se estudió el código local de Code OSS en
`/home/nihuel/projects/personal/refs/vscode`:

- `src/vs/base/browser/ui/menu/menu.ts`: el movimiento del puntero cambia la
  misma fila enfocada que usa el teclado; los estados marcados son independientes
  de ese foco. Code OSS muestra aceleradores de texto en sus menús; OpenIDE aplica ahora
  las teclas con borde también a los menús, por la preferencia explícita del
  producto.
- `src/vs/base/browser/ui/keybindingLabel/keybindingLabel.css`: las ayudas de
  teclado usan teclas individuales de 11 px, con borde, radio de 3 px y
  separadores explícitos.

OpenIDE reutiliza sus propios componentes y colores. Los menús, tooltips,
buscadores y ayudas del editor usan las mismas teclas con borde de 20 px y
radio de 4 px. Cada combinación se mantiene completa y separada de la etiqueta.
Los selectores con contenido rico conservan su jerarquía y su indicación de
valor seleccionado. Los grupos con encabezados se separan con divisores.

## Resultado verificado · 7 de octubre de 2026

El pase final del ejecutable de prueba terminó con **8 de 8 grupos aprobados**,
salida 0 y 12 capturas nativas. Se revisaron visualmente el hover de «Ver», el
menú del editor, el submenú compacto, el scroll en una ventana de 360 px de alto
y el editor vacío estrecho. Las etiquetas y las combinaciones de teclas se leen completas;
las filas marcadas no añaden otro fondo de hover. El menú largo se mantiene
bajo su disparador y permite alcanzar la última acción.

- Evidencia de la ejecución: `/tmp/openide-menu-native/report.json`.
- Capturas: `/tmp/openide-menu-native/menu-*.png`.
- Registro nativo: `/tmp/openide-menu-native/native-menu.log`, sin salida.
- Proyecto aislado de ese pase: `/tmp/openide-menu-e2e-e5t__o54`.
- Render: eframe/wgpu sobre Wayland, KWin virtual con Hyprland anidado.

El pase se ejecutó con `/tmp/openide-tooltip-e2e-bin2`, que incluye los tooltips
direccionales y las teclas compartidas. Los menús usan el ancho estándar de
352 px, limitado por el viewport; las filas cortas conservan 26 px de altura.
La suite actual comprueba cada tecla de `Ctrl+Shift+P` y
`Ctrl+Shift+E/F/G/X`, su borde y su separación respecto de la etiqueta.
Los archivos de `/tmp` son evidencia local temporal; el script reproduce el pase.

La validación central de la misma revisión aprobó 1551 pruebas de la
aplicación (0 fallos, 64 ignoradas) y 461 pruebas de `openide-ui`; la
producción se compiló sin `editor-e2e`.
