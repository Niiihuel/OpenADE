# Validación del depurador nativo

## Adaptadores reales y configuración reproducible

La comprobación inicial del entorno no encontró `lldb-dap`, `codelldb`, `lldb`
ni el módulo Python `debugpy`. Se obtuvieron dependencias en el store de Nix,
sin modificar el perfil, los archivos del usuario ni la configuración del sistema:

```sh
rtk nix-build --no-out-link --expr 'let pkgs = import <nixpkgs> {}; in pkgs.python3.withPackages (ps: [ ps.debugpy ])'
rtk nix-build --no-out-link '<nixpkgs>' -A llvmPackages.lldb
```

Rutas comprobadas en esta máquina:

- Python 3.13.13 con debugpy 1.8.21:
  `/nix/store/fal9f7ff5zg603w0dyl6nmasn25x9mhr-python3-3.13.13-env/bin/python3`.
- LLVM LLDB DAP 21.1.8:
  `/nix/store/vm0kqfwrpqawadci2f9h3v2gcnsindqr-lldb-21.1.8/bin/lldb-dap`.

Estas rutas son locales; cada equipo debe indicar su ejecutable real. El
adaptador y sus argumentos son datos separados, no un comando de shell. La
configuración por sí sola no ejecuta ni compila el programa: iniciar una sesión
requiere la acción explícita del usuario.

Ejemplo Python de `.openide/debug.json`, con el ejecutable del adaptador explícito
y un objeto `arguments` propio de debugpy:

```json
{
  "version": 1,
  "configurations": [
    {
      "name": "Python local",
      "adapter": {
        "command": "/absolute/path/to/python-with-debugpy",
        "args": ["-m", "debugpy.adapter"],
        "id": "debugpy"
      },
      "request": "launch",
      "arguments": {
        "program": "${workspaceFolder}/main.py",
        "cwd": "${workspaceFolder}",
        "python": ["/absolute/path/to/python-with-debugpy"],
        "console": "internalConsole",
        "redirectOutput": true,
        "justMyCode": true,
        "subProcess": false,
        "stopOnEntry": false
      }
    }
  ]
}
```

Para Rust, el adaptador es el ejecutable `lldb-dap` y `arguments.program` apunta
al binario previamente compilado con símbolos de depuración. El probe usa
`rustc -g -C opt-level=0` sobre una fuente temporal propia, y configura
`cwd`, `stopOnEntry: false` y `disableASLR: false`. No compila ni ejecuta el
repositorio del usuario. Las opciones del adaptador se verificaron contra la
[documentación oficial de LLDB DAP](https://lldb.llvm.org/use/lldbdap.html).

## Prueba de protocolo con procesos reales

`scripts/debug-adapter-probe.py` pasó contra ambos adaptadores. El test abre
stdin/stdout enmarcados, lanza una pequeña fixture temporal y comprueba:

- initialize, launch y configuración después del evento initialized;
- breakpoint confirmado por el adaptador y lectura real de variables locales;
- entrar en función, paso siguiente, salir y evaluar el contador resultante;
- continuar, recibir salida, pausar y desconectar terminando el programa.

El contador comienza en 3 y termina en 4. La ubicación se toma del adaptador:
debugpy puede detenerse en la expresión del llamador antes de asignar su retorno;
el DWARF de Rust puede ubicar el paso en la llave final de la función. El test no
reemplaza estas respuestas por posiciones sintéticas. Al desconectar se espera
el evento terminated, se cierra stdin y se recoge el proceso del adaptador.

Reportes y trazas de las corridas reales:

- `/tmp/openide-debugpy-probe/report.json` y `trace.json`.
- `/tmp/openide-lldb-probe/report.json` y `trace.json`.

Reproducción:

```sh
rtk python3 scripts/debug-adapter-probe.py --python /absolute/python-with-debugpy --output /tmp/openide-debugpy-probe
rtk python3 scripts/debug-adapter-probe.py --lldb-dap /absolute/lldb-dap --output /tmp/openide-lldb-probe
```

El segundo comando requiere `rustc` y un linker en PATH. En NixOS se puede usar
`rtk nix-shell -p stdenv.cc --run 'rtk python3 scripts/debug-adapter-probe.py …'`.
Estos probes comprueban el adaptador y el intercambio DAP; no equivalen a una
prueba del host o de la ventana nativa.

## E2E de la ventana nativa

La ronda inicial de validación automatizada central terminó con **1.371 pruebas de la aplicación
aprobadas** y **15 del crate de depuración**, incluidas las dos pruebas con
adaptadores reales debugpy y LLDB. Las demás suites del workspace quedaron
verdes en la ejecución con `--no-fail-fast`; la aplicación se volvió a ejecutar
y pasó después de corregir sus fallos. Estos resultados se distinguen de los
recorridos de interfaz nativa siguientes.

`scripts/debug-desktop-e2e.py` pasó cuatro recorridos con la aplicación real y
debugpy 1.8.21. La repetición del 6 de octubre de 2026 usó
`/tmp/openide-usage-debug-e2e-bin` e incluyó la reorganización del IDE: inspector
en la barra lateral y salida DAP en la consola inferior. Abrió `main.py` desde
Files; en modo Agent, F5/F9 no abrieron el depurador, agregaron marcadores ni
iniciaron el programa. Al cambiar a IDE conservó documento/revisión y verificó
el punto tenue de hover del gutter sin crear un breakpoint.

F9 creó el marcador pendiente. Abrir Depuración desde el rail y leer la
configuración tampoco ejecutó la fixture. Sólo el clic en Start inició el
proceso. La respuesta real del adaptador produjo un marcador verificado, la
flecha de ejecución, stack y `counter = 3`.

Step Into, Step Over y Step Out recorrieron la función y mostraron `counter = 4`.
El script desplazó la lista mediante el puntero nativo para comprobar que las
variables estuvieran efectivamente visibles. Continue retiró la inspección
obsoleta y recibió stdout; el script seleccionó la pestaña Consola y comprobó
`counter=4` dibujado dentro de su viewport real. Pause obtuvo otro stack y Stop
terminó el PID del programa lanzado. Fuente en disco y buffer quedaron intactos.

En la ronda anterior a esta reorganización del IDE se repitieron también los cinco recorridos de
`scripts/editor-structure-desktop-e2e.py`: selección exacta y resize, plegado,
breakpoints pendientes mediante F9/clic, scopes fijados y edición que invalida
estado obsoleto. Pasaron los cinco, sin cambios de producción. La aserción del
tooltip se actualizó al texto actual y se reforzó con estado `Pending`, ausencia
de línea de ejecución y sesión `Unconfigured`.

El cuarto recorrido reinició la sesión y llegó al mismo breakpoint. Alternó a
Agent durante la pausa: no aparecieron controles/gutter de depuración y F5/F9
no cambiaron estado, documento ni breakpoints. Al volver a IDE conservó la misma
sesión pausada. Luego envió la solicitud real de cierre de ventana mediante el
compositor privado. La aplicación
salió con código 0 y terminó el segundo PID depurado antes de cualquier limpieza
de la fixture. El cierre usa `hl.dsp.window.close`, que envía el evento de cierre
Wayland, no una señal de terminación del proceso. La corrida final comprobó que
ya no existían los PID de la aplicación ni de ambos procesos depurados.

La corrida usa un runtime privado KWin virtual → Hyprland anidado y la ventana
`eframe/wgpu` sobre Wayland. Teclado/puntero verifican el PID y el script rechaza
el compositor habitual. El observador de la feature `editor-e2e` sólo registra
estado real con límites explícitos; no simula pausas, variables o verificación.
No se probaron cuentas, proveedores ni procesos del proyecto del usuario.

Reporte y doce capturas: `/tmp/openide-debug-native/`. El log nativo quedó vacío.
Esta evidencia comprueba funcionalidad; no mide FPS ni equivale a todas las
funciones del depurador de Zed o VS Code. Este recorrido no acredita persistencia
de breakpoints entre arranques, expresiones condicionales, attach ni evaluación
REPL; la consola validada presenta salida del adaptador.

```sh
rtk python3 scripts/headless-desktop-fixture.py \
  --kwin /absolute/path/to/kwin_wayland \
  --output /tmp/openide-debug-private \
  -- python3 scripts/debug-desktop-e2e.py \
  --binary /absolute/path/to/agents-window-with-editor-e2e \
  --python /absolute/path/to/python-with-debugpy \
  --wtype /absolute/path/to/wtype \
  --output /tmp/openide-debug-native
```

## Contratos de referencia

La [descripción primaria de DAP](https://microsoft.github.io/debug-adapter-protocol/overview.html)
define capacidades negociadas, configuración después de initialized y
setBreakpoints como reemplazo completo por fuente. La verificación y cualquier
reubicación proceden del adaptador; los cambios posteriores llegan como eventos.
Las referencias a frames/scopes/variables sólo valen durante su pausa actual.
Estas reglas fundamentan la invalidación generacional del host, sin atribuir
capacidades no anunciadas.

El [cliente DAP de debugpy](https://github.com/microsoft/debugpy/wiki/DAP-Client-reference)
documenta su transporte stdio y la secuencia de lanzamiento tardía. Sus
[opciones de lanzamiento](https://github.com/microsoft/debugpy/wiki/Debug-configuration-settings)
permiten la consola interna y desactivar depuración de subprocesos, utilizados
para mantener la fixture acotada.

Se consultó además el checkout local de Zed:
`references/zed/crates/dap_adapters/src/python.rs` separa configuración del
adaptador, argumentos y arranque; `references/zed/crates/dap/docs/breakpoints.md`
asigna al proyecto la propiedad y sincronización de breakpoints. Son referencias
de responsabilidades, no código incorporado a OpenIDE.

En Code OSS se consultaron `debugSession.ts:1071` (envía breakpoints antes de
configurationDone y desconecta si esa configuración falla) y
`debugModel.ts:1036` (la línea confirmada procede del adaptador; un archivo dirty
no conserva el estado verificado). Fuentes:
[secuencia de sesión](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/contrib/debug/browser/debugSession.ts)
y [modelo de breakpoints](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/contrib/debug/common/debugModel.ts).
La lista de variables de Zed, en
`references/zed/crates/debugger_ui/src/session/running/variable_list.rs:282`,
mantiene scopes y referencias estructuradas en lugar de convertir toda la
inspección en texto plano.
