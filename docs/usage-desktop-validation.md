# Validación visual de Uso y tareas por agente

`scripts/usage-desktop-e2e.py` usa la ventana nativa de OpenIDE y un compositor
privado KWin virtual → Hyprland anidado. Requiere un binario compilado con la
feature `editor-e2e`; rechaza el compositor habitual. El bootstrap sólo se activa
con `OPENIDE_USAGE_DESKTOP_FIXTURE=1`, la marca `.openide-editor-fixture` y un JSON
de fixture pequeño. No forma parte del ejecutable normal.

La fixture publica snapshots sintéticos de Codex, OpenRouter, OpenCode, NVIDIA,
MiniMax y Z.ai; estos tres últimos empiezan desconectados.
No guarda credenciales y elimina claves/tokens del entorno del proceso hijo.
Mantiene inactiva la consulta de uso durante la prueba. Los valores prueban la
presentación y sus estados, no acreditan consumos reales de una cuenta.
Los probes automáticos de `gh` y `git credential-manager` quedan bloqueados por
wrappers privados en el PATH del proceso hijo; los demás comandos Git se delegan
al ejecutable real. Así la construcción de la ventana tampoco consulta las
cuentas GitHub guardadas del usuario.

El historial de tareas se crea en el store temporal: tres tareas nativas, dos
Codex y una con nombre largo. Otra instancia tiene el historial vacío. El gráfico
consume el estado normal de la aplicación, sin insertar comandos de dibujo.

## Cobertura ejecutada

- Ventanas de 1600, 1000, 640 y 520 px: hitboxes uniformes de 148 × 36 px,
  tarjetas de la misma altura por fila, pies alineados y scroll hasta cada cuenta.
- Columnas ordenadas con recuentos 3, 2 y 1; nombre completo y cantidad mediante
  hover real. El estado vacío debe mostrar cero y no inventar columnas.
- Conectar NVIDIA, MiniMax y Z.ai abre cada formulario nativo y Cancelar lo
  cierra, sin escribir una clave ni iniciar un worker de autenticación. Cada
  formulario usa una instancia temporal independiente.
- Las acciones de cuenta externa se miden y se inspeccionan; el script no las
  activa. Su dispatch se cubre por las pruebas de UI/router para no abrir el
  navegador o una cuenta real del usuario.

El observador sólo exporta datos acotados de geometría, texto y estado. Todas las
interacciones posteriores al bootstrap son eventos reales de teclado/puntero.
No hay un canal para mutar estado desde el archivo de observación. La salida
incluye reporte JSON, capturas y logs. Esta validación no mide FPS.

## Referencias del gráfico

La documentación primaria de [barras de Apache ECharts](https://echarts.apache.org/handbook/en/how-to/chart-types/bar/basic-bar/)
describe categorías discretas, longitudes proporcionales a los datos y límites de
ancho para que pocas categorías no produzcan barras excesivamente anchas. Son
criterios visuales; el componente de OpenIDE se dibuja con su motor nativo y no
incorpora ECharts ni un navegador. La guía de [tamaño del contenedor](https://echarts.apache.org/handbook/en/concepts/chart-size/)
fundamenta volver a comprobar el gráfico cuando cambia el espacio disponible.

## Reproducción

Con las bibliotecas Wayland, xkbcommon, Vulkan y OpenGL disponibles:

```sh
rtk python3 scripts/headless-desktop-fixture.py \
  --kwin /absolute/path/to/kwin_wayland \
  --output /tmp/openide-usage-private \
  -- python3 scripts/usage-desktop-e2e.py \
  --binary /absolute/path/to/agents-window-with-editor-e2e \
  --wtype /absolute/path/to/wtype \
  --output /tmp/openide-usage-native
```

## Resultado del 6 de octubre de 2026

La ejecución nativa pasó los cuatro grupos de comprobaciones con
`/tmp/openide-usage-debug-e2e-bin`: seis proveedores a 1600, 1000, 640 y 520 px;
las tres aperturas/cancelaciones de formularios; gráfico con historial 3/2/1 y
hover completo; historial vacío. Se midieron hitboxes de 148 × 36 px y la misma
altura de tarjetas/pies dentro de cada fila. Las barras conservaron la relación
3:2:1, un cero común y 48 px de ancho máximo.

El reporte con geometría está en `/tmp/openide-usage-native/report.json`, junto
con veinte capturas PNG. Se inspeccionaron las tarjetas amplias, el gráfico con
hover a 520 px, el formulario MiniMax y el estado vacío a 640 px. Los cuatro
logs nativos quedaron vacíos. Las cuatro ventanas temporales terminaron y
ninguna fixture creó el directorio de credenciales. Las acciones Abrir cuenta
no se activaron: su navegación externa corresponde a la prueba de router.

La primera corrida detectó un detalle del harness: el dispatcher de Hyprland
alternaba floating al repetir el resize. Se corrigió el script consultando el
estado actual antes de alternarlo y se repitió el recorrido completo; no hizo
falta cambiar la aplicación.


## Plan y catálogo de Codex

`scripts/provider-catalog-desktop-e2e.py` pasó cuatro grupos de comprobaciones
el 6 de octubre de 2026 con `/tmp/openide-provider-catalog-e2e-bin`, en el mismo
compositor privado. Usa una extensión explícita de la fixture (`catalog: true`)
con cuatro modelos sintéticos y Luna seleccionado al final; no acredita acceso
real de una cuenta a esos modelos.

A 1000 y 520 px se midió el badge Plan de ChatGPT: 99,03 × 20 px, ancho de texto
nativo más 16 px, borde redondeado, fondo transparente y ningún hit de botón.
Se comprobó en Uso y, mediante scroll real, en la tarjeta inferior de
Proveedores → Codex. Los botones Usar midieron 42,53 × 32 px y el estado
En uso, 54,81 × 32 px. Los rectángulos completos de ambos textos, calculados
con las mismas fuentes egui del renderer, quedaron dentro de sus bordes. El
estado seleccionado no recibió acciones.

La lista mantuvo Astra → Sol → Terra → Luna, aunque el último estaba
seleccionado. Se verificaron «Catálogo de tu cuenta», la fecha de actualización
y el desplazamiento hasta las cuatro filas. El clic real en Actualizar modelos
usó el pipeline normal sin credenciales: retiró el listado de la identidad
sintética, pasó a catálogo sin verificar y mostró «Conecta Codex para actualizar
sus modelos». No se simuló una verificación nueva exitosa.

Un proxy HTTP local de rechazo bloqueó las consultas anónimas externas del
refresco global. No se escribió ninguna credencial, se ejecutó inferencia ni
se abrió una cuenta externa. El éxito de transporte y el orden de respuestas
HTTP pertenecen a las pruebas de catálogo, separadas de este recorrido visual.

Reporte y nueve capturas: `/tmp/openide-catalog-native/`. Se inspeccionaron
modelos estrechos, badge en la subpágina Codex y el fallo sin credenciales. El
log nativo quedó vacío y la ventana temporal terminó. El observador registra
pintura y geometría sólo bajo la fixture marcada; no añade acciones ni cambia
la lógica del catálogo en el ejecutable normal.

Para reproducir, usar el runner anterior con el script
`scripts/provider-catalog-desktop-e2e.py`, `--output /tmp/openide-catalog-native`
y un binario con la feature `editor-e2e`.
