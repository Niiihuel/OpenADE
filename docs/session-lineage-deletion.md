# Borrado de sesiones nativas con subagentes

`SessionStore::delete` protege el linaje persistido antes de modificar los
metadatos, borradores, decisiones de permisos o eventos de una sesión:

- Rechaza el borrado directo de cualquier sesión hija (`SubagentLink`). El
  estado mutable de esa hija también vive en `subagents.registry.json` y
  quitar sólo sus metadatos dejaría el registro huérfano.
- Para una sesión nativa raíz, obtiene su lease exclusivo y busca hijos
  directos en los metadatos de sesiones. Si tiene hijos, está ejecutándose o
  no se puede verificar el directorio de metadatos, devuelve un error sin
  borrar datos. El lease impide que un worker conforme cree un hijo entre
  la inspección y el borrado.
- Si una eliminación anterior quitó los metadatos del padre pero dejó su log,
  un reintento conserva ese log mientras algún hijo todavía haga referencia
  al ID del padre.
- Una sesión del backend CLI que no es hija conserva el borrado anterior; el
  lease y el escaneo de linaje se aplican al backend nativo.

Para eliminar una raíz nativa con descendientes se usa
`SessionStore::delete_native_tree`; su intención durable y recuperación están
descritas en [borrado coordinado](native-lineage-deletion.md). El borrado
directo sigue rechazando hijos y padres con descendientes.

Pruebas focalizadas en `openide-core` cubren padre e hijo protegidos,
borradores y registro intactos, lease de worker activo, metadatos ilegibles,
y borrado CLI independiente.
