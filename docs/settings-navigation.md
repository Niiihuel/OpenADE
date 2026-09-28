# Settings nativo de OpenIDE

Settings se construye con [`openide-ui`](../crates/openide-ui/src/lib.rs), los tokens Rust de [`theme.rs`](../apps/agents-window/src/theme.rs) y el renderizador de la ventana. [`settings_ui.rs`](../apps/agents-window/src/settings_ui.rs) define 20 rutas en tres grupos: **Básico** (8), **Capacidades del agente** (9) y **Datos y estadísticas** (3). La referencia estructural es [`SettingsPage.tsx`](../references/ZCode/packages/ui/src/SettingsPage.tsx) de ZCode; la implementación no ejecuta sus componentes React ni su CSS. `references/warp/` informa la presentación nativa.

El rail se desplaza de forma independiente del contenido. Por debajo de 760 px de ventana, desaparece y el contenido ofrece «Explorar secciones» y «Volver a OpenIDE». Cada página tiene encabezado, descripción y un indicador **Disponible**, **Parcial** o **Planificado**; el indicador describe la capacidad de OpenIDE, no una promesa de paridad con ZCode. Los avisos de guardado y error se muestran dentro de Settings. La vista de proveedores y modelos usa dos columnas cuando el panel central mide al menos 800 px y se apila en espacios menores. Los componentes reutilizables están en [`settings_parts.rs`](../apps/agents-window/src/settings_parts.rs).

| Grupo | Ruta e indicador | UI conectada y límite |
| --- | --- | --- |
| Básico | General · Parcial | Proyecto activo, campo para cambiar carpeta, opción persistente de reabrir la última carpeta y enlaces a Apariencia y Datos. |
| Básico | Apariencia · Parcial | Paleta oscura fija; interruptor persistente para reducir movimiento. No hay selector de tema. |
| Básico | Proveedores y modelos · Parcial | Catálogo models.dev con caché, búsqueda y actualización; proveedor/modelo nativo, ficha de metadatos, disponibilidad de clave de entorno y endpoint OpenAI compatible personalizado. Sólo los adaptadores Rust admitidos permiten seleccionar un modelo. |
| Básico | Navegador · Planificado | Explica que aún no existe una herramienta de navegador para el agente; no ofrece un interruptor ficticio. |
| Básico | Control del equipo · Planificado | Explica la ausencia de captura e interacción con el escritorio y los permisos que harían falta. |
| Básico | Terminal · Parcial | Selección persistente de shell del sistema, presets disponibles para la plataforma o ruta absoluta de un ejecutable. Se aplica a pestañas nuevas y terminales reiniciadas; no hay preferencias de fuente ni perfil. |
| Básico | Atajos de teclado · Parcial | Lista los atajos nativos implementados. Todavía no se pueden reasignar. |
| Básico | Búsqueda de archivos · Parcial | Abre el centro de comandos para buscar archivos. Las reglas de exclusión son internas y no se editan en Settings. |
| Capacidades del agente | Agente · Parcial | Muestra el backend activo, enlaza el modelo Rust y guarda permisos individuales para cinco herramientas nativas. Escritura y comandos habilitados piden aprobación por ejecución. |
| Capacidades del agente | Memoria · Parcial | Describe el recorte del contexto reciente. No guarda recuerdos entre conversaciones ni ofrece visor de memoria. |
| Capacidades del agente | Subagentes · Planificado | Informa que no existe un registro ni ejecución delegada configurable. |
| Capacidades del agente | Plugins · Planificado | Informa que Marketplace instala skills de instrucciones, no plugins de proceso; enlaza Skills. |
| Capacidades del agente | MCP · Planificado | Informa que aún no se configuran ni ejecutan servidores MCP. |
| Capacidades del agente | Skills · Disponible | Muestra instaladas/activas y abre Marketplace para instalar y gestionar skills. El editor reside en la página de workspace. |
| Capacidades del agente | Comandos · Parcial | Abre el centro de comandos de la aplicación. No hay comandos personalizados reutilizables del agente. |
| Capacidades del agente | Automatizaciones · Disponible | Muestra tareas activas/totales y abre el editor del proyecto. El planificador funciona mientras OpenIDE está abierto. |
| Capacidades del agente | Hooks · Planificado | Explica que todavía no se ejecutan acciones alrededor de eventos del agente. |
| Datos y estadísticas | Datos y sesiones · Parcial | Recuento de tareas, agrupación por proyecto/grupo/recientes y orden por creación/actualización, con preferencias persistentes. Archivar y eliminar tareas se hace desde la barra lateral. |
| Datos y estadísticas | Uso · Planificado | Muestra el recuento de tareas locales. No mide tokens ni costes; el precio de catálogo no equivale a gasto real. |
| Datos y estadísticas | GitHub · Parcial | Estado, conexión/verificación de cuenta y perfil; no hay administración general de cuentas ni desconexión desde esta página. |

Las preferencias de movimiento, última carpeta, shell y última sección abierta se guardan en [`settings-preferences.json`](../apps/agents-window/src/settings_preferences.rs). Los permisos de herramientas se guardan por separado en [`agent-tool-settings.json`](../apps/agents-window/src/agent_tool_settings.rs), y la selección del proveedor nativo en [`native-provider.json`](../apps/agents-window/src/native_provider.rs). Las claves de modelos se leen de variables de entorno; Settings sólo indica si la clave elegida está presente, sin comprobar la autenticación.

Las secciones planificadas tienen páginas explicativas, pero carecen de acciones de configuración hasta disponer de un servicio Rust real. El detalle de capacidad y dependencias está en [settings-capability-map.md](settings-capability-map.md).
