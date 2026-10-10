# Panel inferior: validación nativa

`scripts/ide-bottom-desktop-e2e.py` ejecuta OpenIDE en un compositor Wayland
privado. La prueba crea un proyecto y terminales PTY propios, sin cuentas,
credenciales ni servicios externos. El observador de `editor-e2e` sólo lee
los comandos de dibujo, las métricas reales de la fuente y las regiones
interactivas; todas las acciones entran por teclado y puntero nativos.

## Cobertura

- Terminal, Problemas, Salida y Consola: etiquetas centradas horizontal y
  verticalmente en su área de clic, sin solapes entre pestañas. Los botones
  de la barra comparten su centro vertical.
- Hover, selección y cambio de sección: se verifica el fondo pintado, el
  destino visible y que la terminal conserve su PID al volver.
- Una sola barra antes del cuerpo: la primera línea real del PTY aparece
  tras el margen vertical compacto. La prueba escribe una secuencia ANSI
  que limpia la pantalla y coloca un marcador en la primera fila; no
  recorta ni elimina saltos de línea producidos por el shell.
- El selector de sesiones cambia entre procesos independientes sin crear
  otra barra vacía encima del prompt.
- Un arrastre cerca del borde superior de las filas 1 y 2 selecciona
  exactamente sus marcadores ASCII. La coordenada horizontal usa la fuente
  nativa medida; la segunda fila detecta un desfase de ratón con el margen viejo.
- Las dos terminales divididas alinean su primera fila y envían el teclado
  al proceso correcto.
- A 520 px, las cuatro pestañas compactas siguen accesibles y el menú de
  sesiones permanece dentro de la ventana. Escape devuelve el foco al botón.
- Maximizar, restaurar, ocultar y reabrir conserva la terminal y su foco.
- La presentación de Agents conserva las pestañas de sesión de 28 px y
  la misma separación compacta respecto de la cuadrícula del terminal.

El archivo fuente debe quedar idéntico. Los comandos sólo escriben archivos
de PID y directorio de trabajo dentro del proyecto temporal de la prueba.

## Referencias de Code OSS

La separación entre navegación del panel, barra de acciones y terminal se
contrastó con el código local en `/home/nihuel/projects/personal/refs/vscode`:

- `src/vs/workbench/browser/parts/panel/panelPart.ts`: la distribución descuenta
  explícitamente el borde del panel antes de asignar espacio al contenido.
- `src/vs/workbench/browser/parts/paneCompositePart.ts`: navegación y herramientas
  distribuyen el ancho de la barra; el área de contenido tiene su propia geometría.
- `src/vs/workbench/contrib/terminal/browser/media/terminal.css`: la cuadrícula
  xterm vive dentro de su wrapper, con margen horizontal propio para el gutter.

OpenIDE conserva sus pestañas redondeadas y su renderer nativo. La comprobación
evalúa la geometría del resultado, sin depender del DOM o de xterm.

## Ejecución

Compilar una copia con `--features editor-e2e` y ejecutar:

```sh
rtk python3 scripts/headless-desktop-fixture.py \
  --kwin /ruta/a/kwin_wayland --output /tmp/openide-bottom-private \
  -- python3 scripts/ide-bottom-desktop-e2e.py \
  --binary /tmp/openide-bottom-e2e-bin --wtype /ruta/a/wtype \
  --output /tmp/openide-bottom-native
```

`report.json` se escribe sólo si toda la suite pasa. `failure.json` conserva
el error, las mediciones y el frame observado al fallar. Las capturas PNG y
el registro nativo se guardan junto al informe. La producción se compila sin
la función `editor-e2e`.

## Resultado verificado · 7 de octubre de 2026

El binario `/tmp/openide-bottom-e2e-bin` aprobó **8 de 8 grupos**, con salida 0,
12 capturas PNG y un registro nativo vacío. Se inspeccionaron visualmente la
terminal y Problemas, el split, la selección de texto, las pestañas compactas,
el selector de sesiones y la barra de sesiones de Agents.

El pase contra el ejecutable anterior reprodujo el defecto: «Terminal» tenía
un desfase horizontal de 8,34 px y su primera línea quedaba a 14,01 px del
inicio del cuerpo. En el resultado final las cuatro etiquetas tienen un
desfase horizontal de 0 px y vertical de 0,09 px según las métricas nativas;
la primera línea queda a 6,01 px del cuerpo, que incluye el margen de 4 px
y el centrado de la fuente dentro de una celda de 18 px. Las dos terminales
divididas conservan la misma coordenada vertical.

Los arrastres físicos junto al borde superior de las filas 1 y 2 devolvieron
exactamente `FIRST_ROW` y `SECOND_LINE`. Los cambios de sección, sesión,
tamaño y visibilidad conservaron los procesos verificados y los bytes del
archivo fuente.

- Informe: `/tmp/openide-bottom-native/report.json`.
- Capturas: `/tmp/openide-bottom-native/bottom-*.png`.
- Registro: `/tmp/openide-bottom-native/native-bottom.log`.
- Proyecto aislado: `/tmp/openide-bottom-e2e-tt_l8mj2`.
- Reproducción del defecto anterior: `/tmp/openide-bottom-baseline/failure.json`.

Las medidas corresponden a este renderer y fuente nativos, a 1400 × 900 y
520 × 640. Los artefactos de `/tmp` son temporales; el script reproduce las
comprobaciones sin enviar eventos al escritorio del usuario.
