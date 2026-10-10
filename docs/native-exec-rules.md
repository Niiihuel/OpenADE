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

## Vista previa en Settings

La vista previa recibe el ejecutable y sus argumentos como un arreglo JSON
de cadenas. **Evaluar reglas guardadas** vuelve a leer y validar el archivo
desde disco antes de llamar a `ExecPolicy::evaluate_argv`. Conserva el editor,
la política de ejecución, los permisos y el archivo guardado. No inicia un
proceso ni concede una aprobación.

El resultado presenta los argumentos completos, el digest de ese snapshot,
la decisión de las reglas, todas las coincidencias con su patrón y motivo y
la clasificación conservadora de intérpretes. **Sin coincidencias** deja
vigente la política general; tampoco indica que el comando esté autorizado.
Los grants y las aprobaciones de sesión se comprueban durante la ejecución.
La revisión corresponde a las reglas guardadas en ese momento; un turno
anterior conserva la identidad de su política original.

La entrada está acotada a 64 KiB y el evaluador aplica sus límites de argumentos.
JSON inválido, argumentos vacíos, NUL y un archivo corrupto muestran un error.
Cambiar la entrada invalida el resultado anterior. La revisión puede detectar
cambios externos al archivo sin sustituir el borrador local. Los patrones de
las reglas y el informe se presentan completos en superficies con scroll.

Las pruebas `preview_uses_saved_rules_after_reopen_and_preserves_files_and_policy`,
`preview_rejects_invalid_arguments_without_a_stale_report` y
`rules_preview_ui_reads_saved_policy_without_execution_or_permission_changes`
cubren guardado/reapertura, coincidencias múltiples y la más restrictiva,
ausencia de coincidencias, intérpretes, cambios externos y corrupción sin
reescritura. El recorrido de acciones de la app verifica que una entrada de
shell que produciría un archivo no lo ejecuta y que la política de permisos
conserva su digest. `rules_preview_complete_report_and_actions_fit_small_and_large_settings`
comprueba el informe íntegro y las acciones por pointer a 360/900 px. Estas
pruebas evalúan el motor propio local; no cuentan como inferencia con proveedor
ni como aceptación de ejecución de comandos.
Las cinco pruebas propias de esta vista pasaron en Linux el 2026-09-29; esto
no sustituye los gates completos del workspace ni la aceptación con un modelo
real del roadmap.

Esta implementación usa prefijos `argv` de `run_command` y `start_command`.
No implementa el lenguaje Starlark `execpolicy` de Codex ni analiza scripts de
shell incrustados.
