# OpenCode, ACP y OpenChamber: frontera del motor de OpenIDE

Revisión: 28 de septiembre de 2026. Esta es una decisión de arquitectura para
implementar y verificar, no una integración ACP ya terminada. OpenIDE conserva
su interfaz y framework visual en Rust.

## Conclusión

OpenCode es un buen candidato a **motor principal de las conversaciones locales**
de OpenIDE. Empezaría con `opencode acp` para comprobar la experiencia de punta
a punta. Conservaría el harness Rust existente como backend alternativo y para
las funciones cuya ejecución deba controlar OpenIDE. No invertiría ahora en
reproducir en él todo el bucle de OpenCode o Codex.

La elección de **OpenCode como motor** y la de **ACP como transporte único** son
decisiones distintas. ACP cubre el trabajo central del agente, pero no expone
toda la administración que necesita una UI con capacidades como OpenChamber.
Si esas funciones se vuelven requisitos del primer lanzamiento, hay que evaluar
la API HTTP/SSE de OpenCode V2 como transporte principal de ese backend, con
versión fijada y pruebas de contrato. Su [superficie HTTP se declara
experimental](https://opencode.ai/v2/docs/api/); el cliente publicado también
se presenta como [beta](https://opencode.ai/v2/docs/build/client/).

## Qué demuestra OpenChamber

[OpenChamber](https://github.com/openchamber/openchamber) mantiene UI y flujos
propios y usa OpenCode para ejecutar agentes. Su
[frontera de runtime](https://github.com/openchamber/openchamber/blob/main/AGENTS.md#runtime-boundaries)
indica que la UI invoca las APIs oficiales OpenCode 2.x mediante
`@opencode/client`; un [wrapper](https://github.com/openchamber/openchamber/blob/main/packages/ui/src/lib/opencode/client.ts)
resuelve transporte, directorios, errores y proyección al modelo de la app.
Las capacidades que posee OpenChamber pasan por sus propias RuntimeAPIs.
La [propuesta ACP de OpenChamber](https://github.com/openchamber/openchamber/issues/2010)
sigue abierta: el producto actual demuestra la separación UI/motor, **no** que
ACP alcance para replicar toda su interfaz.

| Capacidad | Quién la aporta en OpenChamber | Implicación para OpenIDE |
| --- | --- | --- |
| Inferencia, herramientas, proveedores, agentes, permisos, reglas, skills y MCP | OpenCode; [ACP publica sus capacidades de sesión](https://opencode.ai/v2/docs/cli/acp/). | Mostrar eventos y permisos en la UI Rust; OpenCode conserva la autoridad del agente. |
| Sesiones, streaming y modelo por conversación | OpenCode y la proyección de OpenChamber. | ACP cubre crear/listar/cargar/reanudar/forkear, cancelar y cambiar modelo/esfuerzo/modo de sesión. Guardar en OpenIDE un identificador `engine + external_session_id` y sólo el estado visual propio. |
| Worktrees y ejecuciones paralelas | OpenChamber crea worktrees y coordina sesiones; [Multi-run](https://docs.openchamber.dev/multi-run/) ejecuta variantes en paralelo. | El orquestador Rust crea/gestiona worktrees y sesiones ACP por directorio. El motor ejecuta cada agente. |
| Goals y tareas programadas | El [servidor de OpenChamber](https://docs.openchamber.dev/session-goals/) mantiene el ciclo de seguimiento. | OpenIDE necesita un servicio/scheduler propio si el trabajo debe continuar con la ventana cerrada. ACP no implementa ese producto. |
| Diff review, Git y walkthrough | UI y backend OpenChamber; consulta de diff de sesión a OpenCode y [herramientas Git propias](https://docs.openchamber.dev/git/). | Git local Rust puede cubrir estado/diff/revisión; un diff exacto por turno o revert de OpenCode requiere su API o un contrato adicional. |
| Preview, browser y acceso remoto | [OpenChamber](https://docs.openchamber.dev/preview/) gestiona previews y transporte a otros clientes. | Son servicios de OpenIDE; `opencode acp` es un subproceso local, no un servidor ACP remoto. |

## Límite de ACP frente a la API OpenCode V2

La [documentación de ACP V2](https://opencode.ai/v2/docs/cli/acp/) especifica
JSON-RPC v1 sobre `stdio`. El proceso crea un servidor **privado** y puede
atender varias sesiones; no se conecta al servicio compartido ni publica un
puerto ACP. Por eso no se debe asumir que un `opencode serve` separado observe
el estado en vivo de ese proceso. Su almacenamiento de sesiones puede ser
común, pero eso no convierte ambos servidores en una sola autoridad de ejecución.

| Necesidad de Settings o de la experiencia | ACP | API HTTP/SSE V2 |
| --- | --- | --- |
| Turnos, streaming, herramientas, solicitudes de permiso, uso y cancelación | Sí; son parte del flujo documentado. | Sí; con eventos y endpoints de sesión. |
| Lista de sesiones, carga, reanudación y fork | Sí, según las capacidades anunciadas por el agente. | Sí, además de más operaciones de administración. |
| Modelo, esfuerzo y modo de la sesión | Sí, entre las opciones habilitadas que anuncia OpenCode. | Sí; también lista modelos, proveedores y agentes. |
| Alta de proveedor, credenciales y OAuth desde Settings | ACP usa credenciales ya guardadas por OpenCode; `opencode auth login` es el camino documentado. | Tiene endpoints de integración, credenciales y configuración; verificar flujo con una versión fijada. |
| Administrar MCP y permisos guardados | ACP admite servidores MCP al abrir una sesión y resuelve permisos que llegan al cliente. | Ofrece listado, conexión y configuración MCP, además de permisos pendientes/guardados. |
| Diff completo por turno, revert, Git y estado global | ACP transmite llamadas de herramientas, pero no ofrece esos métodos globales. | Endpoints de diff/revert/VCS; la UI Rust también puede resolver Git localmente. |
| Proceso compartido o remoto | Subproceso local privado por `stdio`. | Servidor HTTP con autenticación y transporte de eventos. |

Las columnas de la API derivan de su [OpenAPI V2 actual](https://opencode.ai/v2/docs/api/).
Cada versión instalada debe comprobarse: el
[wrapper de OpenChamber](https://github.com/openchamber/openchamber/blob/main/packages/ui/src/lib/opencode/client.ts)
ya documenta cambios de rutas y formas entre versiones 2.x. No diseñar el
modelo persistente de OpenIDE a partir de tipos wire de OpenCode.

## Responsables en OpenIDE

1. **UI y dominio Rust:** navegación, Settings, preferencias de presentación,
   compositor, Git, worktrees, previews, automatizaciones y proyección de
   eventos. `openide-ui` continúa siendo el único framework visual.
2. **OpenCode:** bucle del agente, proveedores/credenciales, herramientas,
   permisos internos, sesiones y selección de modelo de sus conversaciones.
   OpenIDE muestra los permisos que llegan por ACP y envía la respuesta al
   agente; los toggles del harness nativo no modifican OpenCode.
3. **Models.dev:** catálogo informativo y caché independiente para búsqueda,
   capacidades y precios aproximados. Para ejecutar con OpenCode, el selector
   debe usar los modelos **habilitados que OpenCode anuncie**, no cualquier ID
   recién publicado en Models.dev.
4. **Harness Rust nativo:** backend alternativo, automatizaciones propias y
   funciones que requieran control directo de OpenIDE. Mantener su seguridad y
   corrección, pero priorizar paridad sólo donde haya una necesidad concreta.
5. **Codex CLI:** perfil externo existente. El código de Codex sirve como
   referencia de diseño; ver
   [`codex-rust-harness-reference.md`](codex-rust-harness-reference.md).

## Implementación verificable

1. Extraer de `agents-window` un contrato Rust de backend de conversación
   (sesión, turno, delta, herramienta, permiso, uso, cancelación, error). La UI
   recibe ese contrato sin depender de tipos ACP ni de OpenCode. Persistir la
   pareja `engine + external_session_id` y la metadata visual, evitando una
   segunda transcripción autoritativa para sesiones externas.
2. Crear adaptador ACP Rust para `opencode acp`: detectar ejecutable/versión,
   iniciar proceso con `cwd`, negociar capacidades, abrir/cargar sesión, enviar
   prompt, proyectar eventos, responder permisos, cancelar, cerrar y recuperar
   tras reinicio. Probar primero en Windows con una versión fijada.
3. Ejecutar una prueba real de aceptación: sesión nueva y reanudada, streaming
   de texto/razonamiento/herramientas, un permiso aprobado y uno rechazado,
   cambio de modelo/esfuerzo/modo, imagen/archivo si se anuncian, MCP, corte de
   proceso y recuperación. Medir qué datos faltan para Settings y revisión Git.
4. En Settings distinguir **motor** de **proveedor/modelo**. La página OpenCode
   muestra instalación, estado, versión, credenciales delegadas, modelos
   habilitados y configuración accesible. La página del harness nativo mantiene
   sus adaptadores y claves por variable de entorno. Los estados `Parcial` y
   `Planificado` permanecen hasta que exista el servicio correspondiente.
5. Si el contrato ACP deja una función necesaria sin resolver, elegir una ruta
   explícita: implementarla en Rust (por ejemplo Git/worktrees), abrir el flujo
   propio de OpenCode (login), o reemplazar **ese backend** por `opencode serve`
   con API/SSE fijada. Evitar operar ACP y un servidor HTTP independiente como
   si fueran la misma instancia.

**Criterio para declarar OpenCode motor principal:** el recorrido completo del
paso 3 funciona y la UI recupera sesiones tras reinicio sin perder permisos ni
atribuir eventos a otra sesión. Hasta entonces, la integración sigue siendo
una candidata, y el harness/CLI actuales continúan operativos.
