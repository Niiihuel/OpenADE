# Recuperación de subagentes nativos

Un hijo es una sesión nativa con journal y lease propios. `SubagentWorker::run`
admite un único turno con el ID de spawn y, si el proceso cae, el coordinador
muestra su estado como `Interrupted` después de observar que se liberó el lease
del hijo. `list_children_for_ui(parent_session_id)` permite leer ese estado sin
tomar el lease del padre ni escribir el registro; valida el linaje y los datos
del registro antes de entregar la lista.

`SubagentWorker::resume` y `resume_prepared` sólo reclaman un hijo
`Interrupted`. Comprueban que coincidan padre, ID de hijo, ID de spawn, prompt,
modelo, modo, entorno del modelo, catálogo y política de hooks. El worker
construye `ResumeEngine` con el mismo proveedor, endpoint, instrucciones,
catálogo, permisos, límites, presupuesto y hooks. El lease del hijo permanece
tomado durante la recuperación. Nunca se admite otro turno para continuar el
anterior.

El catálogo que verifica la recuperación es el que ofrece su propio ejecutor,
en el orden del ejecutor: el mismo que confirmó el turno en vivo. La identidad
de la reserva (`request_digest`) conserva el orden de `catalog_for_mode`. Los
dos órdenes difieren cuando el hijo tiene a la vez herramientas de goals y de
subagentes, que es lo que el desktop da a cada hijo.

La recuperación avanza hasta un evento terminal confirmado o devuelve una
pausa tipada:

- `NeedsPermission` conserva el ID de solicitud y vuelve a pedir decisión.
- `ToolOutcomeUncertain` conserva el ID de llamada iniciada sin resultado; no
  invoca la herramienta otra vez. Hace falta verificar el efecto y registrar
  un resultado explícito.
- `AbortedToolBatch` conserva la siguiente llamada de un lote terminado de
  forma anormal; no ejecuta el resto automáticamente.

La pausa se guarda como `Interrupted` antes de soltar el lease. Un nuevo intento
de recuperación ve la misma llamada y no duplica efectos. Un error de identidad
o journal corrupto bloquea la ejecución. La respuesta del modelo ya confirmada
se reutiliza; una petición de modelo en vuelo puede repetirse con el mismo paso
durable. Un checkpoint presente exige la misma configuración de compactación
para verificarlo.

Los métodos de resolución humana de `ResumeEngine` y el registro manual de
resultados inciertos siguen siendo pasos separados; esta API entrega las pausas
a la UI, pero todavía no ofrece un flujo completo de decisión para cada pausa
del subagente. Tampoco prepara herramientas MCP para hijos: el catálogo actual
es el ejecutor local suministrado por el host. Los hooks, cuando el host los
configura con `with_command_hooks`, ejecutan con permisos del usuario y su
huella forma parte de la identidad inmutable del spawn.
