# Comandos de administrador en Linux

El selector **Permitir todo** habilita las herramientas del agente con la política configurada por el host. La opción de administración agrega `elevated: true` a `run_command`; cada operación elevada se autoriza mediante el agente gráfico de Polkit. La aplicación y sus herramientas de archivos siguen ejecutándose como el usuario de escritorio. Las reglas `Ask` y `Deny` de comandos mantienen su efecto sobre el comando y sus argumentos originales.

La capacidad se activa únicamente con `LocalTools::with_elevated_commands(true)`, permiso `run_command` y aislamiento `HostUnrestricted`. El catálogo no anuncia `elevated` para otros perfiles. El journal y la validación previa de herramientas siguen ocurriendo en el runner antes de llegar al ejecutor; el wrapper privilegiado se construye después de esas fronteras.

## Autenticación y requisitos

Se requiere Linux, Polkit/pkexec instalado por el sistema y un agente gráfico de autenticación asociado a la sesión. El ejecutor busca exclusivamente un `pkexec` propiedad de root, ejecutable, con bit setuid y sin escritura de grupo/otros en `/run/wrappers/bin/pkexec` (NixOS), `/run/current-system/sw/bin/pkexec`, `/usr/bin/pkexec` o `/bin/pkexec`. No obtiene el programa de autenticación desde el proyecto ni desde un `PATH` modificable. `linux_administrator_commands_available()` comprueba este prerrequisito mediante metadatos; no autentica ni certifica que la política de la sesión vaya a conceder la operación.

Usa `pkexec --disable-internal-agent --user root ...`. Polkit muestra su diálogo de autenticación; OpenADE no solicita, recibe, registra ni envía contraseñas. Según la [documentación oficial de pkexec](https://polkit.pages.freedesktop.org/polkit/pkexec.1.html), el código 126 indica un diálogo cancelado y el 127 indica autorización ausente o error. Ambos terminan la operación con un resultado explícito, sin ejecutar el comando como usuario para aparentar éxito. No se instala una política que conceda elevación silenciosa ni se cambia la política de autenticación del sistema.

En la máquina usada para estas pruebas Polkit y su agente gráfico ya están presentes, pero `/run/current-system/sw/bin/pkexec` tiene modo `555 root:root`, sin setuid, y no hay wrapper en `/run/wrappers/bin/pkexec`. Por eso la comprobación de administración devuelve `false`. Las pruebas no modificaron la configuración de NixOS ni realizaron operaciones como root.

El [módulo oficial de NixOS](https://github.com/NixOS/nixpkgs/blob/master/nixos/modules/security/polkit.nix) configura el wrapper mediante `security.polkit.enablePkexecWrapper`, separado de habilitar el daemon. Una receta declarativa para revisar y aplicar desde la configuración de sistema es:

```nix
security.polkit.enable = true;
security.polkit.enablePkexecWrapper = true;
```

La receta no se aplicó automáticamente y no agrega reglas de autorización silenciosa.

## Supervisor y cancelación

Antes de iniciar eframe, ventanas o credenciales, el ejecutable debe llamar a `run_elevated_command_helper_from_env()` y salir inmediatamente si devuelve un código. La llamada también registra la frontera de inicio ordinario: el ejecutor rechaza hosts que no integraron este entrypoint, para evitar abrir la aplicación entera como root. Este modo especial ejecuta únicamente el supervisor de un comando; exige UID efectivo root y `PKEXEC_UID`, valida argumentos y usa rutas absolutas. Los límites son 256 argumentos, 64 KiB y timeout de 1 a 120 segundos. No usa shell implícito y el stdin del comando está cerrado.

El stdin privado entre el proceso de usuario y el supervisor root funciona como una concesión de vida. EOF por cancelación, timeout, cierre de la aplicación o abandono del futuro cancela el comando desde el supervisor privilegiado, que puede matar su grupo de procesos root. El proceso de usuario no presupone que tiene permiso para enviar señales a root. Si la concesión ya terminó durante el diálogo de autenticación, el supervisor comprueba EOF antes de iniciar el comando. Un timeout o un resultado de supervisor inválido se registra como efecto incierto; no debe reintentarse automáticamente.

La salida conserva los límites normales de `run_command` y se transporta mediante un protocolo JSON acotado a 1 MiB. No se heredan credenciales ambientales del escritorio. Como con los comandos normales de host, un programa que deliberadamente crea otra sesión/grupo puede escapar del grupo supervisado: este modo concede administración real y no constituye una sandbox para programas hostiles.

`start_command`, stdin interactivo y sesiones persistentes no admiten elevación. El catálogo rechaza el argumento antes de iniciar un proceso. El soporte inicial administra sólo comandos acotados, con autenticación por operación y sin captura de secretos.

## Verificación

Las pruebas herméticas usan un ejecutable falso local para representar la frontera Polkit, nunca una cuenta root. Comprueban los permisos y el aislamiento, el schema, las reglas sobre argv real, argumentos literales, rutas absolutas, rechazo/cancelación de autenticación sin fallback, EOF de la concesión al cancelar, timeout con efecto incierto, cliente ausente y protocolo inválido. Las pruebas existentes de grupos de procesos verifican la limpieza del comando que reutiliza el supervisor. La integración con el agente gráfico de una distribución concreta debe comprobarse en una sesión con Polkit instalado.
