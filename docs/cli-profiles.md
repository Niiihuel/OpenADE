# Agentes CLI

Agents Window incluye perfiles para Codex, Claude Code, Gemini CLI y ZCode.
Muestra cuáles ejecutables están disponibles en `PATH`. Cada run lanza un
proceso en el directorio de trabajo elegido, envía el prompt y presenta
stdout/stderr y estado final. La autenticación sigue a cargo de cada CLI.

Puedes agregar o reemplazar perfiles creando `cli-profiles.json` en la carpeta
de datos de OpenIDE (`%APPDATA%\\OpenIDE` en Windows; `$XDG_DATA_HOME/openide` o
`~/.local/share/openide` en Linux). Consulta
[cli-profiles.example.json](../config/cli-profiles.example.json). Los campos son:

| Campo | Significado |
| --- | --- |
| `id` | Identificador único; reemplaza un perfil integrado si coincide |
| `label` | Nombre mostrado en la barra lateral |
| `executable` | Comando en `PATH` o ruta al ejecutable |
| `args` | Argumentos literales; `{prompt}` solo en modo `argument` |
| `env` | Variables de entorno específicas del proceso |
| `prompt_transport` | `stdin` o `argument` |

Los perfiles con `stdin` evitan interpolar el prompt en un shell. En Windows,
el modo `argument` rechaza los wrappers `.cmd` y `.bat`; usa un ejecutable
nativo o un perfil que acepte el prompt por stdin.

La CLI externa puede tener su propia política de aprobación y sus propias
herramientas. El interruptor de herramientas de la ventana controla únicamente
el harness nativo de OpenIDE: habilita escritura de archivos dentro del
workspace y ejecución de comandos iniciados allí. Los comandos heredan los
permisos del usuario del sistema; el directorio de trabajo no los aísla.
