# Marketplace de skills

El Marketplace de OpenIDE acepta una carpeta local o un repositorio de GitHub indicado expresamente por la persona usuaria. Busca carpetas con `SKILL.md` en `.agents/skills`, `.claude/skills`, `skills` y en la raíz del origen. Exige `name` y `description` en el frontmatter, valida que `name` coincida con la carpeta y muestra la licencia cuando está declarada.

Las instalaciones se copian a los datos privados de OpenIDE con ámbito de usuario o de proyecto. El agente nativo lee las skills activadas con límites de cantidad y tamaño. Las CLIs externas conservan sus propias reglas de descubrimiento. El Marketplace no carga complementos JavaScript/TypeScript, paquetes npm ni servidores MCP.

## Flujo y estados

- Sin orígenes: estado vacío con acción para agregar una carpeta o URL GitHub.
- Origen agregado: catálogo local con búsqueda por nombre, descripción u origen; filtros de todas o instaladas; selector de ámbito antes de instalar.
- Skill instalada: estado, switch de activación y desinstalación. Quitar un origen no borra las copias instaladas.
- Origen ausente: la instalación sigue visible con su estado real; el catálogo muestra el diagnóstico del origen.
- Actualización: releer el catálogo local es inmediato; `MarketplaceStore::refresh_github_sources()` vuelve a clonar los repositorios configurados y valida los manifiestos antes de sustituir el caché. Debe ejecutarse en un worker y sólo desde una acción explícita de actualizar, no en cada render.

La UI vive en `apps/agents-window/src/marketplace_view.rs`; el inventario y la persistencia están en `apps/agents-window/src/marketplace.rs`. Los IDs de acción empiezan por `marketplace_` y los resultados de operaciones regresan al estado de la vista antes de volver a construir el árbol de UI.
