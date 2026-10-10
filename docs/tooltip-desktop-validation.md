# Tooltips direccionales: validación nativa

`scripts/tooltip-desktop-e2e.py` abre un proyecto aislado, sin cuentas ni
credenciales, en un compositor Wayland privado. Reutiliza el observador de
menús de la función `editor-e2e`: sólo lee el frame realmente pintado. El
puntero, la rueda y el teclado pasan por dispositivos virtuales privados;
no se envían eventos al escritorio del usuario.

## Qué comprueba

- IDE: ayuda a la derecha del rail y de las filas del explorador; debajo de
  los controles del encabezado; encima de la barra de estado.
- Agents: ayuda debajo del encabezado y del dock, a la derecha del engranaje
  lateral, a la izquierda de los archivos del dock y encima del contexto.
- A 520 × 360, las ayudas de los bordes superior, inferior e izquierdo quedan
  completamente dentro de la ventana y separadas de su disparador.
- Cada ayuda aparece después del retraso, desaparece al retirar el puntero,
  conserva el foco y deja disponible el área de clic del control.
- La geometría se toma del rectángulo pintado y de la fuente nativa medida,
  no de la preferencia declarada por el componente.
- El menú «Más» del editor conserva los grupos separados por divisores y
  muestra `Ctrl+D` y `Ctrl+Space` con las mismas teclas de 20 px y radio 4 px
  que los tooltips. Se recorre con la rueda y se verifica que las teclas
  queden dentro del menú y separadas de sus etiquetas.

El pase usa controles reales y verifica que el archivo del proyecto no cambia.
El selector de permisos a 520 × 640 fuerza además una inversión real: su
ayuda prefiere la izquierda, pero el menú está junto a ese borde. La prueba
exige que aparezca a la derecha y fuera de toda la superficie del menú, sin
activar ni modificar permisos. Las pruebas unitarias del motor amplían la
cobertura de las combinaciones de lados y tamaños.

## Ejecución

Compilar una copia de OpenIDE con `--features editor-e2e` y ejecutar:

```sh
rtk python3 scripts/headless-desktop-fixture.py \
  --kwin /ruta/a/kwin_wayland --output /tmp/openide-tooltip-private \
  -- python3 scripts/tooltip-desktop-e2e.py \
  --binary /tmp/openide-tooltip-e2e-bin --wtype /ruta/a/wtype \
  --output /tmp/openide-tooltip-native
```

`report.json` sólo se escribe si todos los casos pasan. Un fallo conserva la
observación y los casos completados en `failure.json`, con una captura del
frame que falló. Las capturas y registros se generan en el directorio de salida.
El ejecutable de producción se compila sin `editor-e2e`.

## Revalidación de Kbd · 7 de octubre de 2026

El ejecutable `/tmp/openide-kbd-e2e-bin` aprobó **17/17 casos** con salida 0
y 18 capturas después del cambio a teclas de 20 px de alto y radio de 4 px.
El observador comprobó esas medidas en el frame nativo de tooltips y menús,
además de la separación de los textos, los divisores y los límites del menú.

Se inspeccionaron el tooltip de Archivos, las dos capturas desplazadas del
menú del editor y la inversión de ayuda a la derecha en 520 px: las teclas
están completas, los grupos separados y la ayuda no cubre el selector.

- Evidencia: `/tmp/openide-kbd-tooltip-native/report.json`.
- Capturas: `/tmp/openide-kbd-tooltip-native/tooltip-*.png`.
- Registro: `/tmp/openide-kbd-tooltip-native/native-tooltip.log`, vacío.
- Proyecto aislado: `/tmp/openide-tooltip-e2e-7_5lxvv2`.

La misma compilación pasó **9/9 grupos** de perfiles, paleta y Vim, con
16 capturas y cero bytes filtrados al PTY. Este pase no vuelve a certificar
las suites unitarias ni la suite de menús de la revisión anterior.

## Resultado anterior · 7 de octubre de 2026

El ejecutable final `/tmp/openide-tooltip-e2e-bin2` aprobó **17 de 17 casos**,
con salida 0 y 18 capturas nativas. Se inspeccionaron las imágenes finales de
Archivos a la derecha del rail, los atajos y divisores del menú del editor y
la inversión lateral junto al borde. Las ayudas y teclas se leen completas;
la ayuda de permisos cabe en 520 px sin tapar el menú ni su fila.

- Evidencia: `/tmp/openide-tooltip-native/report.json`.
- Capturas: `/tmp/openide-tooltip-native/tooltip-*.png`.
- Registro: `/tmp/openide-tooltip-native/native-tooltip.log`, sin salida.
- Proyecto aislado: `/tmp/openide-tooltip-e2e-vuts3ebb`.

En el mismo binario pasó también la suite de menús: **8 de 8 grupos** y
12 capturas. Ambos pases conservaron los bytes originales del archivo y
no usaron cuentas ni credenciales. Los artefactos de `/tmp` son temporales;
los scripts permiten reproducir estas verificaciones.

La validación central de la misma revisión aprobó **1551 pruebas de la
aplicación** (0 fallos, 64 ignoradas) y **461 pruebas de `openide-ui`**.
El ejecutable de producción se compiló sin la función `editor-e2e`.
