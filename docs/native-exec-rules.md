# Reglas de comandos del agente nativo

Settings → Comandos permite guardar reglas de prefijo para `run_command` y
`start_command`. Cada patrón es un arreglo JSON de argumentos exactos. El
primer token identifica al ejecutable; los siguientes se comparan con el
comienzo de `args`. Una lista dentro de un token expresa alternativas:

```json
["git", ["push", "reset"]]
```

Las decisiones editables son **Pedir aprobación** y **Bloquear**. Las reglas
no habilitan una herramienta ni amplían el permiso base: la decisión más
restrictiva entre la configuración de la herramienta y todas las reglas
coincidentes determina el resultado. El harness trata conservadoramente los
wrappers de shell y los intérpretes, porque su argumento puede contener un
segundo comando que no se puede clasificar por su `argv` externo. La entrada
interactiva de un proceso persistente también se controla por separado.

Las reglas se guardan en `native-exec-rules.json` dentro de la carpeta de datos
de OpenIDE. El archivo tiene versión, límite de 64 KiB y escritura atómica.
Una versión desconocida, un archivo corrupto, un enlace o una regla inválida
bloquean la carga y el editor muestra el error. El runtime recibe únicamente
la última configuración validada y guardada. El digest de la política se
registra antes de la primera petición al modelo de cada turno; reanudar un
turno con reglas distintas requiere restaurar su configuración anterior.
Ante un archivo inválido, Settings ofrece **Restablecer reglas** con una
confirmación explícita para reemplazarlo por una política vacía.

Esta implementación usa prefijos `argv` de `run_command` y `start_command`.
No implementa el lenguaje Starlark `execpolicy` de Codex ni analiza scripts de
shell incrustados.
