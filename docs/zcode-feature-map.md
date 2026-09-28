# Mapa funcional de ZCode

Este inventario sirve para portar comportamiento sin copiar implementación. Está
basado en el clone `references/ZCode` (commit
`29628c9acdb81b703bbd4080c207a0e7ce5e276e`).

| Área en ZCode | Ubicación de referencia | Objetivo en OpenIDE |
| --- | --- | --- |
| Estado del agente y turnos | `apps/zcode-cli/packages/core/src/runtime/` | `openide-harness`: turno, bucle de modelo, cancelación y tool calls |
| Contratos y eventos | `apps/zcode-cli/packages/contracts/` | `openide-core`: mensajes y eventos serializables |
| Sesiones e historial | `apps/zcode-cli/packages/adapters/src/storage/` | `openide-core`: historial durable y reanudación |
| Proveedores de modelos | `apps/zcode-cli/packages/core/src/model/` | Backend registrable; primero API OpenAI compatible |
| Herramientas y permisos | `apps/zcode-cli/packages/core/src/tool/`, `permission/` | Registro de herramientas, validación de ruta y política de aprobación |
| Contexto, memoria y compactación | `context/`, `memory/`, `compact/` | Construcción de contexto y compactación por presupuesto de tokens |
| Skills, hooks, MCP y plugins | `hooks/`, `mcp/`, `plugin-reference/` | Extensiones con interfaces estables y aislamiento |
| Subagentes y workflows | `subagent/`, `workflow/` | Orquestación de tareas y eventos de hijos |
| Navegador y REPL | `browser-client/`, `repl/` | Herramientas opcionales con controles de permiso |
| Gateway y clientes | `bootstrap/`, `packages/server`, `packages/desktop`, `packages/web`, `packages/tui` | Ventana nativa Rust y, después, protocolo para clientes remotos |

## Orden de implementación

1. Flujo funcional completo: ventana, sesión, modelo compatible con OpenAI,
   herramientas locales, eventos y cancelación.
2. Integraciones de CLIs por perfiles configurables, con stdout/stderr y estado
   del proceso.
3. Política de permisos persistente, cola de turnos con deduplicación y
   reanudación de sesiones.
4. MCP, skills, hooks, memoria, compactación y subagentes.
5. Workflows, herramientas de navegador y protocolo remoto.

Este documento registra la distancia entre la versión en desarrollo y ZCode;
no presenta la primera versión como paridad completa.

## Estado verificado de esta versión

- `openide-harness` implementa permisos independientes para lectura, listado,
  búsqueda, escritura y comandos. El valor inicial habilita sólo herramientas
  de lectura. Falta la interacción de aprobación por llamada y la persistencia
  de decisiones de ZCode.
- La política de contexto del harness limita el tamaño aproximado de cada
  petición, retira turnos antiguos completos y acorta resultados de
  herramientas antiguos sin romper sus identificadores. Falta estimación por
  tokens reales y síntesis semántica con el modelo.
- `openide-cli` detecta perfiles instalados en `PATH`, los ejecuta en el
  workspace y transmite salida y cancelación. La ventana sólo muestra perfiles
  detectados; la CLI externa mantiene su propia autenticación y permisos.
- La ventana implementa una primera estructura Zai Dark con sidebar de tareas,
  timeline y compositor inferior. Sus capacidades de edición y presentación
  siguen siendo menores que las de la UI v4 de ZCode.
- El dock de sesiones usa un selector compacto Grupo/Proyecto, una vista
  cronológica accesible desde el filtro y grupos manuales con creación,
  renombre, orden, membresía, colapso y persistencia validada en Rust. El
  encabezado ofrece plegar/reabrir la barra con `Ctrl+B`. Los controles de
  sólo icono principales usan un tooltip propio con puntero y atajo opcional.
  Aún faltan drag and drop, colores de grupo y las demás pantallas de ZCode.
