# Aislamiento de comandos en Windows

`CommandIsolation::WorkspaceWriteOffline` ejecuta comandos mediante el ProcessContainer de [Microsoft MXC](https://github.com/microsoft/mxc), fijado al commit `ff53fda4a88463370891cd4a6a0b1aa06acae9e5` y distribuido bajo licencia MIT. El harness exige un workspace canónico en una unidad fija local y construye una política con escritura concedida a esa ruta, red denegada por defecto, entorno limitado a variables de arranque y un Job Object que termina el árbol de procesos. Si MXC no puede preparar o lanzar el contenedor, el comando falla sin ejecutarse en el host. La implementación está en [`command_sandbox.rs`](../crates/openide-harness/src/command_sandbox.rs).

La elección de aislamiento proviene de `LocalTools::with_command_isolation(...)`, no de los argumentos del modelo. La biblioteca conserva `HostUnrestricted` como valor predeterminado para compatibilidad: su Job Object controla el árbol de procesos, pero no restringe archivos ni red. Este perfil también limita el entorno heredado a variables de arranque, locales y rutas de herramientas; no entrega automáticamente claves API, tokens, sockets de autenticación ni otras variables arbitrarias al comando. En Windows, `AgentToolSettings::default()` de la aplicación selecciona `WorkspaceWriteOffline` cuando se habilita `run_command`; la preferencia se puede cambiar desde la configuración de herramientas. En macOS y demás plataformas sin adaptador, ese perfil falla antes de iniciar un proceso. Linux tiene un [adaptador propio](linux-command-sandbox.md) con requisitos y evidencia separados; una build Linux no vuelve a verificar el backend MXC.

## Verificación en este equipo

Las pruebas ejecutadas con el backend real de Windows comprobaron que un proceso mínimo puede escribir dentro del workspace y que el sistema deniega lectura y escritura de un archivo hermano fuera de él. También deniega esas operaciones a través de una junction del workspace hacia el directorio hermano. Un intento TCP a `127.0.0.1` no llegó al listener. Una cancelación terminó al descendiente antes de su escritura diferida. Un timeout posterior a una escritura devuelve `EffectUncertain` y termina el proceso antes de la siguiente escritura. La ACL visible del workspace, medida con `icacls`, coincidió antes y después de la salida normal, cancelación y timeout. El entorno del hijo MXC no contenía un canario secreto arbitrario ni `OPENAI_API_KEY`; el perfil HostUnrestricted tampoco recibió un canario arbitrario. El dispatcher de `LocalTools` también se probó con el perfil MXC y un intento de escritura fuera del workspace. El harness trata cualquier advertencia de política o limpieza de MXC como resultado incierto tras iniciar el proceso.

Ejecutar estas verificaciones con el toolchain Windows GNU:

```powershell
$env:PATH = "$env:USERPROFILE\.local\toolchains\msys2-mingw64-binutils\mingw64\bin;$env:PATH"
& "$env:USERPROFILE\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu test -p openide-harness workspace_offline
& "$env:USERPROFILE\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu test -p openide-harness command_isolation_choice_reaches_executor
```

## Alcance

La prueba de lectura cubre un secreto hermano y una junction hacia él en este host; no demuestra que todos los archivos externos posibles sean ilegibles en todas las versiones de Windows. La prueba de red cubre loopback; la política MXC se configura con red denegada, pero falta una prueba de conexión externa. Algunos ejecutables pueden necesitar DLL o permisos que el contenedor no concede y entonces fallan cerrados. MXC administra permisos temporales del contenedor y selecciona el mecanismo compatible con el host. Comandos que dependan de variables de entorno personalizadas necesitarán un mecanismo futuro de configuración explícita y confiable. `WorkspaceWriteOffline` no se debe describir como activo para una ejecución que use `HostUnrestricted`.

Como referencia arquitectónica, Codex implementa su propio token, ACL de workspace, lanzamiento y filtros WFP en [`windows-sandbox-rs`](../references/codex/codex-rs/windows-sandbox-rs/src); OpenIDE usa el SDK de MXC para el límite OS y mantiene propio el harness, el catálogo de herramientas y la UI.
