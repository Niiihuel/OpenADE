# Borrado coordinado de sesiones nativas

`SessionStore::delete_native_tree(root_id)` elimina una sesión nativa raíz y
todos sus subagentes. `SessionStore::delete` sigue rechazando hijos y padres
con descendientes. La operación nueva exige que `root_id` no tenga un padre.

## Protocolo

1. Obtiene un lock de borrado por raíz y el lease exclusivo de cada sesión del
   árbol. Escanea hijos después de tomar el lease de cada padre, de modo que un
   worker conforme no puede agregar un hijo entre el escaneo y la eliminación.
2. Toma el lock compartido con el coordinador de subagentes para
   `subagents.registry.json`. Cada hijo debe tener estado `completed`, `failed`
   o `cancelled`; no puede conservar entradas de seguimiento `pending` ni
   `active`. Un lease ocupado o un estado no terminal aborta antes de mutar.
3. Persiste `.native-delete-intents/<root_id>.json` dentro del directorio de
   estado con los IDs y relaciones del árbol.
   Cada ID de hijo se verifica contra el ID determinista de su padre y
   `spawn_id`.
   El intento bloquea nuevas adquisiciones de leases, admisiones nativas,
   creación de hijos y escrituras nativas para cualquiera de esos IDs.
4. Quita del registro los estados y entradas del árbol; limpia sus borradores.
   Después elimina eventos, permisos y metadatos en orden descendiente antes
   del padre. Borra el intento sólo al terminar. Los locks de lease permanecen
   como archivos estables. Ningún archivo del workspace entra en la operación.

El método devuelve `true` cuando completa un borrado nuevo o pendiente, y
`false` cuando la raíz ya no existe y no queda intento. Si el proceso cae,
`pending_native_deletion_roots()` lista los IDs a recuperar y
`recover_pending_native_deletions()` reintenta cada uno. El manifiesto se
valida antes del reintento, incluidos IDs, relaciones y metadatos aún presentes.
También se detectan hijos nuevos fuera del manifiesto. Un registro ilegible o
un linaje inconsistente bloquea el borrado para una reparación explícita.

El host debe llamar a `recover_pending_native_deletions()` al abrir el almacén
antes de recuperar workers o mostrar esos árboles. Las APIs de worker de
`SessionStore` rechazan los IDs del intento pendiente, pero el host debe evitar
mutaciones directas de archivos y mantener sus propias barreras de estado
para operaciones UI. La transacción de varios archivos es recuperable; no es
una operación atómica de sistema de archivos. En Windows, la eliminación de
entradas de directorio no tiene un `fsync` portátil; el orden descendiente y el
manifiesto permiten terminar un borrado interrumpido en la siguiente apertura.

El registro de subagentes conserva su versión 1 o 2 y cualquier campo ajeno
al árbol. La dependencia de esquema entre `openide-core` y el coordinador de
subagentes se limita a `version`, `children`, `inputs`, `status` y `state`.
