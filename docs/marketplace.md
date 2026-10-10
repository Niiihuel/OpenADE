# Marketplace de skills

El Marketplace de OpenIDE presenta recomendaciones integradas, búsqueda de la comunidad en skills.sh y gestión de instalaciones. También permite agregar una carpeta local o un repositorio de GitHub. Las skills son paquetes de instrucciones `SKILL.md`; no son complementos JavaScript/TypeScript, paquetes npm ni servidores MCP. Navegar por el catálogo no descarga ni instala contenido.

## Catálogo inicial y procedencia

El manifiesto `apps/agents-window/assets/marketplace/starter-catalog.json` incluye 36 skills de seis publicadores con nombre, descripción, licencia, repositorio, carpeta, categoría, revisión Git inmutable y lista completa de archivos con tamaño y SHA-256. Las categorías son Diseño y frontend, Calidad y pruebas, Datos, Nube y despliegue, Flujo de desarrollo y Documentación. La interfaz deriva las categorías del manifiesto, sin mantener otra lista fija.

Las tres entradas anteriores conservan su identidad: `frontend-design` de [Anthropic](https://github.com/anthropics/skills), `vercel-react-best-practices` y `vercel-composition-patterns` de [Vercel](https://github.com/vercel-labs/agent-skills). Cada detalle identifica su revisión exacta; navegar por las recomendaciones funciona sin conexión.

Los detalles muestran procedencia, revisión, licencia, tamaño y notas de compatibilidad. Abrir el repositorio es una acción explícita. La instalación descarga únicamente la lista declarada desde esa revisión, comprueba tamaños y hashes y valida el frontmatter antes de publicar la copia instalada. No ejecuta scripts ni instala dependencias. La procedencia exacta queda guardada en `.openide-source.json`. Una actualización de OpenIDE puede ofrecer un manifiesto nuevo; no sustituye silenciosamente las copias instaladas.

## Orígenes propios y persistencia

Un origen propio busca carpetas con `SKILL.md` en `.agents/skills`, `.claude/skills`, `skills` y en su raíz. Exige `name` y `description` en el frontmatter y valida el nombre de la carpeta. Las instalaciones se copian a los datos privados de OpenIDE con ámbito **Usuario** o **Proyecto**. El agente nativo lee las skills activadas con límites de cantidad y tamaño; las CLIs externas conservan sus propias reglas de descubrimiento.

Las skills activadas desde Marketplace se presentan al agente de forma progresiva: al inicio sólo se incluye un índice con nombre, descripción y rutas, limitado a 128 skills y 64 KiB. El agente lee `SKILL.md` y sus referencias mediante `read_file` cuando necesita su contenido. Cada `SKILL.md` admite como máximo 256 KiB, en coherencia con el límite de esa herramienta. Las skills locales seleccionadas manualmente en la configuración conservan su mecanismo de carga previo; no se convierten automáticamente a este índice.

El índice incluye la ruta absoluta del paquete para resolver sus referencias. El harness concede lectura a esas carpetas únicamente mediante `read_file` y conserva el permiso de lectura configurado; listar, buscar, escribir y ejecutar comandos mantienen sus límites habituales. Las referencias externas no activan instrucciones `AGENTS.md` del proyecto.

La búsqueda local filtra por nombre, descripción u origen. **Recomendadas** incluye el catálogo integrado y los orígenes propios, con selector de categoría y opción **Tus orígenes**. **Instaladas** incluye únicamente copias instaladas. Cada instalación tiene estado **Activa/Inactiva**, switch y desinstalación con confirmación. Quitar un origen también requiere confirmación y conserva las copias ya instaladas. Un origen que deja de estar disponible muestra su diagnóstico sin fingir que desapareció la instalación.

**Actualizar catálogo** relee los orígenes locales y actualiza los repositorios GitHub propios desde un worker. La operación no se ejecuta en cada render. El catálogo inicial conserva la revisión fijada por su manifiesto; una revisión nueva del manifiesto puede aparecer como actualización disponible para la copia instalada. Un error deja un aviso y permite reintentar; una operación en curso deshabilita las acciones que podrían competir con ella.

## Explorar y actualizar

**Explorar** mantiene una consulta independiente de los filtros locales. Escribir no hace peticiones: **Buscar** consulta el catálogo público de [skills.sh](https://skills.sh/) con al menos dos caracteres. El worker limita la consulta a 120 caracteres y 100 resultados, y la UI conserva como máximo 24 tarjetas por página. Buscar también se puede activar con Enter desde el campo. El resultado muestra nombre, repositorio y, en el detalle, instalaciones registradas por el catálogo. El número no representa usuarios activos ni una garantía de calidad. No se inventa una descripción extraída de `SKILL.md` antes de descargarlo.

El estado distingue búsqueda en curso, lista vacía, error con **Reintentar**, copia guardada y copia guardada sin conexión. La caché conserva resultados durante 15 minutos antes de pedir una versión nueva; un error puede recuperar una copia anterior con aviso explícito. Cambiar de consulta o proyecto invalida la presentación de respuestas anteriores. Buscar, abrir detalles y comprobar actualizaciones no instala nada.

La instalación remota requiere un clic separado. Resuelve una revisión del repositorio, valida la identidad del paquete y descarga su carpeta con referencias y licencia, dentro de límites de cantidad, tamaño y tiempo. No ejecuta comandos. Los detalles de una instalación muestran repositorio, revisión y carpeta realmente guardados.

**Instaladas** ofrece **Buscar actualizaciones** y marca únicamente diferencias comprobadas. La acción **Actualizar** muestra sus revisiones en los detalles y sustituye la copia cuando el usuario la pulsa; conserva el ámbito Usuario/Proyecto y la activación. No hay actualizaciones silenciosas al abrir la pantalla. Los errores mantienen la instalación anterior y una acción de reintento.

## Presentación nativa

La cabecera agrupa título, actualización y **Agregar origen**. Las pestañas **Recomendadas**, **Explorar** e **Instaladas**, la búsqueda y los filtros son compactos; el ámbito de instalación conserva su etiqueta en ancho reducido. Una sola skill ocupa el ancho del catálogo; varias usan dos columnas cuando hay espacio suficiente. Cada tarjeta contiene identidad, origen, descripción resumida, licencia y una acción clara. **Detalles** conserva la descripción completa y la información de procedencia.

En el IDE, el lateral Complementos concentra las pestañas y la consulta mientras está visible. El catálogo central evita repetirlas y conserva tarjetas, ámbito de instalación y gestión de orígenes. Al ocultar ese lateral, el centro recupera los controles de búsqueda. El campo y su botón comparten una fila de 28 px; en el lateral, el envío es un icono con tooltip y atajo Enter. La búsqueda remota identifica `skills.sh` en el propio campo y distingue consulta inicial, carga, error y resultados vacíos.

La instalación y su ámbito se muestran con un chip pasivo. El estado acompaña al switch compartido; su tooltip describe la acción disponible. En ventanas estrechas, metadatos y controles ocupan filas separadas. Los estados vacíos distinguen ausencia de instalaciones y búsquedas sin coincidencias. Los modales de origen enfocan el campo, admiten Escape y devuelven el foco al botón que los abrió.

La vista vive en `apps/agents-window/src/marketplace_view.rs`; inventario y persistencia en `marketplace.rs`; el manifiesto y la descarga de recomendaciones en `marketplace_catalog.rs`; búsqueda, procedencia y actualizaciones en `marketplace_remote.rs`. `marketplace_app.rs` coordina workers, resultados por proyecto y generaciones de búsqueda. El panel Complementos del modo IDE reutiliza las mismas páginas acotadas y conserva IDs físicos distintos del catálogo central. Los IDs comienzan por `marketplace_`; los resultados del worker actualizan el estado antes de reconstruir la vista.

La reproducción y el alcance de las pruebas de escritorio están en [marketplace-desktop-validation.md](marketplace-desktop-validation.md).
