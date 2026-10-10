# Aislamiento de comandos en Linux

`WorkspaceWriteOffline` tiene un adaptador propio en
`openide-harness/src/command_sandbox_linux.rs`. Bubblewrap aporta primitivas del
sistema operativo; OpenIDE define los montajes, el filtro seccomp, el protocolo
de lanzamiento y la supervisión. Las sesiones nativas no importan bibliotecas
ni ejecutan procesos de Codex u OpenCode.

## Disponibilidad y lanzamiento

`CommandIsolation::is_supported_on_host()` distingue el soporte de la build
(Linux x86_64/aarch64 o Windows) de
`is_available_on_host()`. En Linux, la segunda consulta busca un ejecutable ELF
de Bubblewrap en una ruta absoluta de PATH, sin permisos de escritura para
grupo/otros ni bits setuid/setgid. Al preparar el lanzamiento también se exige
que el helper resuelto esté fuera del workspace writable. La consulta de
disponibilidad no ejecuta una prueba de aislamiento al pintar Settings.

Cada lanzamiento debe preparar namespaces, seccomp, supervisión por pidfd y
descriptores antes de autorizar al ejecutable del modelo. Un wrapper de shell
confiable, ya dentro del aislamiento, publica una señal de preparación y espera
permiso en otra tubería. Sólo después ejecuta el argv literal, sin interpretación
implícita de sus argumentos. Se rechaza antes de preparar un ejecutable cuyo
nombre empieza con `-`, para que el builtin exec del shell no lo interprete como una opción;
un archivo con ese nombre debe seleccionarse mediante una ruta como `./-name`.
Los descriptores del protocolo se cierran antes
del exec: el wrapper cierra los FD 3 y 4; Bubblewrap consume los FD 5 (filtro) y
6 (workspace), y el descriptor fijado del helper, FD 7, se cierra con CLOEXEC.
Los demás FD no estándar heredados también se marcan CLOEXEC. El handshake tiene
un límite de 5 segundos; un fallo de preparación no se reintenta en Host.
Una entrega de permiso o un fallo posterior ambiguos conservan incertidumbre.

Además de Bubblewrap, el equipo necesita user namespaces no privilegiados,
seccomp, pidfd y close_range. La presencia del helper no certifica esos requisitos:
se comprueban al preparar cada proceso. La selección proviene de la política
confiable del host, nunca de un argumento del modelo.

## Archivos, entorno y red

La vista monta el workspace canónico con escritura, las raíces de runtime del
sistema de sólo lectura y un `/tmp` privado. Tiene `/proc` de su namespace de PID
y dispositivos virtuales mínimos. No monta la raíz del host, su `/proc`, `/dev`,
`/etc`, home, `/root`, `/run` de servicios o `/tmp` existente. El HOME del proceso
apunta a un directorio temporal privado. Las raíces de runtime están enumeradas
en el adaptador y pueden contener configuración de compilación del sistema; no
se afirma confidencialidad total de todo dato externo.
El `/proc` nuevo todavía publica metadata global del kernel, además de los PID
del namespace; los dispositivos virtuales como `/dev/null` y `/dev/zero` son
excepciones explícitas a la regla de sólo escritura en archivos del workspace.
La raíz generada se remonta de sólo lectura; `/tmp` y el workspace conservan sus
montajes writable.

El filtro propio deniega sockets de todas las familias, incluidos IPv4/IPv6,
UNIX y VSOCK, las rutas de red de io_uring y mecanismos de ptrace, namespaces y
montajes que podrían ampliar el acceso. Comprueba la arquitectura y rechaza
la ABI x32 en x86_64. No basta con aislar la interfaz de red: un socket UNIX por
pathname también puede comunicar con un servicio del host.

El entorno se limita a las variables de arranque declaradas por el harness y
las rutas temporales privadas; no se heredan claves API o variables arbitrarias.
Una variable como CARGO_HOME no concede acceso al directorio que nombra. Los
toolchains de rustup en home pueden quedar inaccesibles. El ejecutable y sus
dependencias deben estar en el workspace o en los runtimes montados; no se abre
home ni `.cargo/credentials.toml` como solución implícita.

La validación previa rechaza workspaces reservados, aliases no canónicos y
archivos con hardlinks, porque un alias de inode podría modificar un archivo
externo, y montajes anidados bajo el workspace, porque también pueden representar
aliases externos. La lectura de mountinfo tiene un límite de 4 MiB. El recorrido
de archivos está acotado a 100.000 entradas y 64 niveles, con errores
que conservan la ruta y el límite. Ese límite no es un timeout de syscalls de
filesystem. Se comprueba cancelación entre directorios y cada 64 entradas;
la preparación de `run_command` ocurre en un worker de bloqueo. El workspace y
helper se fijan mediante descriptores para el
lanzamiento. Cambios concurrentes del host durante la inspección siguen siendo
un límite; no se presenta este recorrido como defensa completa frente a un host
adversarial que reemplaza ancestros o crea hardlinks durante la ejecución.

## Procesos y UI

El guard usa pidfd para terminar el supervisor sin confundir un PID reutilizado.
La relación de muerte de padre de Bubblewrap termina PID 1 y su namespace, incluso
para descendientes que crean una sesión con setsid. `run_command` y los comandos
persistentes comparten la preparación. Los persistentes usan tuberías, no PTY.
Cancelar, agotar el tiempo, revocar el gestor o cerrarlo termina el aislamiento.
Reabrir los stores nunca restaura procesos.

Settings → Agente presenta el requisito ausente y conserva el perfil guardado.
Si no está disponible, `run_command` y las operaciones persistentes quedan en
Deny y la selección no cambia a Host. Con un perfil disponible, start, stdin y
terminate requieren Ask; poll sigue Allow. Los controles humanos conservan su
confirmación y ledger bajo lease, sin ejecutar desde la UI. Los nuevos catálogos
describen el perfil Linux; un digest histórico incompatible no se acepta como un
catálogo nuevo.

## Evidencia y límites

La aceptación se registra en [el roadmap](native-harness-roadmap.md), con nombres
de pruebas y resultados reales. El host de esta revisión es Linux 7.1.3 x86_64,
con Bubblewrap 0.11.2 obtenido mediante Nix. Aarch64, macOS, equivalencia manual
de Windows y proveedores remotos no quedan certificados por una build Linux.
Los hooks y MCP tienen sus propios contratos: esta selección aísla las
herramientas de comandos, no todos los procesos auxiliares del producto.
El buffer de salida permanece acotado y no es todavía historial incremental
durable.

La suite propia del adaptador contiene una prueba sin requisitos de aislamiento
y cinco aceptaciones OS explícitamente ignoradas por defecto. No se devuelve un
éxito silencioso si faltan los requisitos. En este host se ejecutó la suite
completa, **6 pruebas verdes**, incluyendo las dos ABI alternativas de x86_64:

- `setup_rejects_invalid_helper_and_workspace_alias_without_payload`: helper no
  ELF, alias de directorio y archivo con hardlink rechazados, canary intacta.
- `actual_workspace_sandbox_handshake_executes_literal_argv`: argumento Unicode
  y shell substitution literal, sin efecto adicional; archivo ejecutable llamado
  `-name` funciona mediante `./-name`, mientras `-a`, `-c` y `--` se rechazan
  antes de consultar el helper o recorrer el workspace y sin modificar su salida.
- `actual_filesystem_sockets_and_inherited_descriptors_are_confined`: lectura y
  escritura externas y symlink de escape denegadas; archivo del runtime y raíz
  generada no writable; archivo temporal privado permitido; listener TCP real y
  socket UNIX del host sin conexión; IPv4, IPv6, UNIX, VSOCK e io_uring rechazados
  con EPERM; nueva user namespace denegada; FD host deliberadamente heredable
  cerrado antes del payload; NoNewPrivs=1 y Seccomp=2 observados en el proceso.
- `actual_setup_filter_failure_never_executes_target`: filtro malformado provoca
  fallo real de Bubblewrap antes del payload; cancelación anterior también lo
  evita.
- `actual_pid_namespace_ends_setsid_descendants_on_cancel_timeout_drop_and_exit`:
  el heartbeat escrito por el descendiente con setsid se detiene en los cuatro
  caminos, incluido el retorno normal del padre.
- `actual_seccomp_rejects_x32_and_compat_syscall_abis`: x32 e int 0x80 terminan
  con SIGSYS, antes de escribir el marcador de bypass.

Comando reproducible, con el toolchain y Bubblewrap aportados sólo al proceso de
prueba por Nix:

```sh
rtk env PATH=/home/nihuel/.cargo/bin:/run/current-system/sw/bin:/home/nihuel/.local/bin nix shell nixpkgs#gcc nixpkgs#pkg-config nixpkgs#wayland nixpkgs#libxkbcommon nixpkgs#bubblewrap --command rtk cargo test -p openide-harness command::sandbox::linux::tests -- --include-ignored
```

Esto no verifica los controles manuales, la UI ni las variantes de comandos
persistentes: sus evidencias independientes se agregan en el roadmap.
También se probó el wrapper exacto con `/bin/sh` del host y Dash 0.5.13.3 obtenido
mediante Nix: ambos completan el protocolo de FD y preservan el argumento Unicode
y la sustitución de shell como texto literal. Ese probe de portabilidad no es
una aceptación del aislamiento OS con Dash como `/bin/sh` del sistema.

Fuentes primarias: [Bubblewrap](https://github.com/containers/bubblewrap) explica
que la política depende del caller;
[seccomp BPF](https://docs.kernel.org/7.1/userspace-api/seccomp_filter.html) documenta
el filtro y su herencia; el
[protocolo de Bubblewrap 0.11.2](https://github.com/containers/bubblewrap/blob/v0.11.2/bubblewrap.c)
sitúa seccomp antes del exec y mantiene su reaper, y
[PID namespaces](https://man7.org/linux/man-pages/man7/pid_namespaces.7.html)
documenta la terminación del namespace cuando muere PID 1. La implementación y la evidencia de OpenIDE, no el
nombre de estas primitivas, determinan el alcance de este perfil.
