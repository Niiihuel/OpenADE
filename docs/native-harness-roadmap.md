# Harness nativo de OpenIDE: decisión y criterios de cierre

Decisión: 28 de septiembre de 2026. Referencia de Codex:
[`33a0f766`](https://github.com/openai/codex/tree/33a0f766a647208b471cfbcad889c67fd324ee04),
clonada en `references/codex` dentro del workspace y excluida por Git.

## Decisión

OpenIDE desarrollará y operará su propio harness en Rust en
`openide-harness`, con contratos durables en `openide-core` y presentación en
el framework `openide-ui`. Codex es una referencia técnica; el producto no
importa `codex-rs/core` como biblioteca ni lo necesita como proceso para las
sesiones nativas. El perfil CLI de Codex sigue disponible como integración
externa. La selección de modelos usa un
[catálogo propio](providers-models-architecture.md) obtenido de las APIs
oficiales de los proveedores admitidos, con caché local. El catálogo no
autoriza por sí solo la ejecución.

OpenIDE conserva la propiedad del ciclo del agente en Rust. La comparación con
[Codex](codex-rust-harness-reference.md) informa contratos de turno,
recuperación y herramientas, sin delegar la autoridad del journal o los
permisos de las sesiones nativas.

## Fronteras técnicas

| Capa | Dueño | Regla |
| --- | --- | --- |
| UI y estado visual | `openide-ui`, `agents-window` | Proyecta eventos del dominio y envía comandos con ACK; no ejecuta herramientas ni decide el resultado de un turno. |
| Sesiones y persistencia | `openide-core` | IDs estables de sesión/turno/comando, log ordenado e idempotente, checkpoints y replay. |
| Harness | `openide-harness` | Loop, modos, presupuesto de contexto, compactación, catálogo de herramientas, permisos, ejecución y cancelación. |
| Proveedores | Adaptadores Rust por protocolo y registro local versionado | Listas directas desde APIs oficiales, con mapeos JSON declarativos, última copia local válida y hash de credencial/definición. Los esfuerzos aparecen sólo para el ID exacto y un protocolo que sepa enviarlos. |
| Referencias externas | `references/` | Lectura y comparación. Si se incorpora código Apache de Codex, conservar `LICENSE`, `NOTICE` y atribuciones aplicables. |

## Estado y trabajo pendiente

Las casillas representan una capacidad completa **verificada**, no solo la
existencia de un tipo o un control visual.

| Capacidad | Estado actual | Criterio de cierre pendiente |
| --- | --- | --- |
| Bucle modelo-herramienta, streaming, cancelación y permisos básicos | Parcial | Journal síncrono y prueba HTTP local de Plan→Build→Resume tras efecto incierto→compactación/checkpoint con reapertura de stores; faltan aceptación con proveedor real y cierre explícito de cada estado de interrupción. |
| Admisión durable del turno | Parcial | `TurnAdmitted` idempotente, lease exclusivo entre procesos y secuencia verificada. Un lock transaccional por sesión serializa lectura, admisión, append y borrado entre handles/procesos; el tail cache se invalida al cambiar el journal o recrear un ID. La UI distingue las admisiones no iniciadas y ofrece recuperación. Faltan aceptación con proveedor real y pruebas de más puntos de caída. |
| Modos Plan y Build | Parcial | El modo está persistido y Plan ofrece sólo lecturas locales certificadas. `PlanProposed` y `PlanUpdated` guardan snapshots acotados que sobreviven a compactación y Resume; `update_plan` sólo se anuncia en Build. La UI muestra los pasos y ofrece «Construir este plan» una vez antes de la admisión Build. Faltan aceptación con modelo real y clasificación extensible de efectos. |
| Responses API y proveedores | Parcial | OpenAI Responses persiste IDs, uso y elementos opacos en `ModelResponseCommitted`; Chat Completions conserva ID y uso cuando el stream los entrega, exige `[DONE]` y un `finish_reason` de `tool_calls` o `function_call` antes de ejecutar herramientas, y rechaza `finish_reason=length`. Pruebas HTTP locales. El registro incorpora OpenAI, OpenRouter, DeepSeek, Groq, Mistral y xAI, y admite proveedores añadidos para protocolos implementados. Catálogo y backend usan capacidades de IDs exactos, sin reglas por prefijo; una matriz revisada cubre sólo los pares con evidencia adicional. Faltan pruebas con cuentas reales y ampliar adaptadores verificados. |
| Tokens y compactación semántica | Parcial | Resumen semántico, checkpoint con checksum, comando manual y compactación automática en ejecución y resume con pruebas locales. Las solicitudes Responses con imágenes y límite verificado consultan el conteo exacto de `POST /responses/input_tokens` antes de responder y después de compactar; un conteo desconocido o fallido no admite la solicitud bajo esa puerta. Para modelos sin límites verificados, se permite el turno sin compactación automática y la compactación manual registra esos límites como desconocidos, sin inventar ventana ni reserva. Faltan aceptación con proveedor real, confirmar el endpoint para cada modelo anunciado y ampliar ventanas verificadas. |
| Resume/recovery | Parcial | Replay estricto, estados de recuperación, motor durable por pasos y worker conectado a la UI. El marcador v2 vincula proveedor, endpoint, modelo, modo, workspace, catálogo, instrucciones y perfil exacto de solicitud antes de la primera petición. Las decisiones de permiso registradas antes de una caída se aplican una vez al reanudar el padre o hijo. La barra lateral y la sesión seleccionada muestran recuperación para turnos raíz e hijos; los lotes abortados se descartan explícitamente y los efectos inciertos se verifican manualmente. Un journal inválido muestra diagnóstico sin reintento automático. Se reconstruyen catálogos históricos sin `send_input`, sin PTY y anteriores a `update_plan`. MCP HTTP moderno remoto HTTPS con bearer u OAuth en Windows fija identidad autenticada; HTTP legacy y loopback siguen cerrados para resume. Faltan aceptación de punta a punta con proveedor real y pruebas con servidor MCP y vault reales. |
| Registro de herramientas y ejecución | Parcial | Catálogo congelado, digest persistido por turno y validación JSON Schema Draft 2020-12 antes de pedir permiso o ejecutar. El scheduler agrupa hasta cuatro lecturas locales certificadas, sincroniza cada inicio antes del efecto y confirma resultados en orden; el replay exige verificar manualmente las lecturas sin resultado tras una caída. `apply_patch` nativo acepta Add/Delete/Update y `Move to:` de texto acotado bajo el mismo permiso que `write_file`, preevalúa el lote sobre un estado virtual, valida contextos y destinos antes de cambiar archivos y revierte archivos y directorios creados si un commit reporta fallo. La reversión de varios archivos no es una transacción atómica ante corte de energía ni frente a reemplazos concurrentes hostiles. Mutaciones y MCP siguen seriales. Faltan trazas completas y clasificación extensible de efectos. |
| Aislamiento de comandos | Parcial | Windows tiene perfil MXC de escritura sólo en workspace y red denegada, con pruebas de proceso real para archivo hermano, junction, loopback, entorno, cancelación y restauración de ACL. Los comandos sin sandbox reciben un entorno mínimo, sin secretos arbitrarios heredados. `start_command`/`poll_command`/`write_stdin`/`terminate_command` mantienen procesos acotados por sesión nativa en modo host, y los revocan al retirar acceso. Las reglas de prefijo argv Ask/Deny para `run_command` y `start_command` tienen editor en Settings, carga fail-closed y huella durable; una recuperación valida la misma huella. Los fallos ambiguos después de iniciar un proceso quedan como efecto incierto. Faltan perfiles equivalentes en otras plataformas y pruebas de red externa. |
| Comandos persistentes | Parcial | El harness tiene gestor `start`/`poll`/`write_stdin`/`terminate` con Job Object en Windows, límites de vida y salida, marcadores de omisión y pruebas de procesos reales. ConPTY opt-in funciona en Host y permite redimensionamiento dinámico; el catálogo legacy sigue disponible por digest para Resume. En Windows, `WorkspaceWriteOffline` usa MXC persistente con tuberías, sin PTY, y pruebas reales de aislamiento de archivos, red, junctions y descendientes. `LocalTools` anuncia las herramientas con `run_command` habilitado en Host o MXC; Settings pide aprobación para start, stdin y terminate. La app conserva un gestor por sesión nativa, muestra procesos en Actividad y permite terminarlos sin drenar su salida. Faltan equivalentes aislados en otras plataformas y aceptación interactiva con un proveedor real. |
| MCP, skills, hooks y extensiones | Parcial | Clientes stdio y Streamable HTTP, descubrimiento, catálogo, ajustes editables en UI y runtime conectado al runner Build. HTTP negocia 2026-07-28 o el ciclo de sesión de 2025; reanuda SSE legacy con `Last-Event-ID` sin repetir POST. Las notificaciones `tools/list_changed` en SSE de respuesta y GET legacy, y una renovación de sesión HTTP después de descubrir herramientas, invalidan el catálogo de forma persistente y bloquean llamadas futuras. Stdio sin credenciales y HTTP moderno HTTPS remoto, incluido bearer y OAuth en Windows con identidad ligada al vault, pueden fijar una huella acotada para resume; HTTP legacy y loopback siguen fail-closed. OAuth MCP tiene descubrimiento, PKCE, refresh, almacén en Windows Credential Manager y flujo conectado a Settings/runtime; faltan aceptación con servidor OAuth y vault reales. Marketplace activa sus skills instaladas; Settings permite elegir skills locales por workspace y la activación revalida el contenido antes de cada turno. Su digest de procedencia entra en las instrucciones y el marcador de contexto para resume. Hooks Pre/Post configurados en Settings tienen ejecución acotada, huella durable y recuperación conservadora de efectos inciertos; Plan excluye lecturas con hooks. Faltan más notificaciones MCP y aceptación completa de extensiones. |
| Instrucciones de proyecto | Parcial | `AGENTS.md` y `AGENTS.override.md` se cargan en orden desde la raíz Git/workspace con límites de niveles, archivos y bytes, y rechazo de enlaces en toda la cadena. La app inicia turnos con cwd en la raíz del workspace. Antes de pedir al modelo, un evento durable guarda versión, texto exacto y digest con límite de 512 KiB. Resume del padre e hijo usa esa copia aunque cambien los archivos. `read_file`, `write_file`, `list_files`, `search_files` y `apply_patch` detectan reglas anidadas del destino antes de operar, guardan la instantánea durable ligada a la llamada y la reproducen después de una caída o compactación. Comandos y herramientas externas aún no tienen ese preflight; faltan marcadores configurables y aceptación con proyectos reales. |
| Subagentes y coordinación | Parcial | Hijos nativos durables con `spawn_id` idempotente, parentesco, límites, lease propio, ejecución concurrente, cancelación persistida y herramientas Build `spawn_agent`, `send_input`, `wait_agent`, `cancel_agent`. `send_input` usa una cola durable con reintentos idempotentes y recuperación tras varios puntos de interrupción. El control opaco deja avanzar el siguiente turno padre mientras el hijo corre; la app instala el wrapper, muestra sesiones hijas y recibe eventos persistidos mediante un bridge. Las herramientas hijas usan los permisos de escritura/comandos del usuario y presentan aprobaciones, cancelación y recuperación en la UI del padre o hijo; pueden crear descendientes con profundidad y cantidad acotadas. El archivado verifica descendientes no terminales y entradas pendientes; el borrado coordinado de la línea nativa usa intención durable y recuperación al iniciar la app. Faltan estado y acciones completas en UI y aceptación con modelo real. |
| Permisos por sesión | Parcial | El journal guarda de forma atómica la decisión y un grant exacto ligado al workspace, política, herramienta, argumentos y aislamiento. Runner, Resume y subagentes validan el grant antes de reutilizarlo; la UI ofrece «Permitir en esta sesión» para padre e hijo. La recuperación del hijo puede persistir el grant antes de ejecutar. Actividad muestra los grants de la sesión y permite revocarlos; la decisión histórica conserva la marca de revocación y las llamadas siguientes vuelven a pedir permiso. Hay pruebas de persistencia, reapertura y acción UI. Falta aceptación con proveedor real. |
| Intervenir durante un turno activo | Parcial | Una cola durable por turno registra instrucciones e imágenes con ID idempotente, valida lease y replay al confirmarlas, y un fence durable evita cerrar el turno con entradas pendientes. Runner y Resume las incorporan en límites seguros entre solicitudes y herramientas; hay pruebas de carrera y reapertura. La UI pone instrucciones en cola durante un turno nativo y conserva un nonce estable del borrador para reintentos ambiguos. Falta aceptación interactiva con proveedor real. |
| Entrada y salida multimodal | Parcial | La UI admite imágenes del proyecto con límites de 8 MiB por archivo, ocho por mensaje y 24 MiB agregados, y verifica capacidad por modelo. Core persiste bytes, MIME y hash en admisión o steering; replay/checkpoint los preservan, Runner/Resume envían la imagen exacta del journal, y Responses usa `input_image` con data URL. Chat Completions rechaza imágenes explícitamente. El conteo exacto del proveedor alimenta el presupuesto previo cuando hay ventana verificada; no se inventa una tarifa local por modelo. Pasan pruebas locales de transporte, corrupción y reapertura; faltan salida multimodal y aceptación con proveedor real. |
| Perfiles de aislamiento y herramientas alojadas | Parcial | El perfil Windows MXC cubre workspace offline. Responses permite búsqueda web alojada opt-in en modos caché, índice o vivo; conserva acciones, fuentes y citas en `provider_output_items`, vinculadas a la huella de petición para Resume. Settings activa el modo para OpenAI y la UI muestra citas clicables desde el journal. Faltan permisos diferenciados de lectura/denegación, perfiles aislados en otras plataformas y aceptación con proveedor real. |
| Reintentos del proveedor | Parcial | Chat Completions y Responses reintentan como máximo dos veces 429, 5xx, desconexiones y fallos de transporte antes del primer delta, con `Retry-After` acotado y cancelación. Tras el primer delta, un corte queda como error terminal para no duplicar texto. Pruebas de backend/Responses y Clippy pasan; falta aceptación con proveedor real. |
| UI y Settings | Parcial | Todos los controles que anuncian capacidades están conectados a servicios reales y presentan estados honestos. |

## Orden de implementación

1. Hacer durable la admisión y el log: ID de turno, ACK, eventos idempotentes,
   replay y recuperación del borrador. Esta es la base para que la UI y el
   runtime acuerden qué trabajo existe.
2. Establecer el protocolo de modelo y el modo por turno: Responses, uso real,
   Plan/Build y una matriz explícita de capacidades por proveedor.
3. Implementar compactación y resume con checkpoints, control de presupuesto
   y pruebas de interrupción en cada punto de efecto externo.
4. Completar herramientas: registro, validación, scheduler, sandbox por
   plataforma, MCP y políticas de permisos por sesión/proyecto.
5. Añadir subagentes, skills/hooks y proyección completa a la UI propia.
   Pasar una aceptación de punta a punta en Windows, incluidas sesiones
   recuperadas y modelos reales compatibles.

El objetivo permanece abierto hasta que todos los criterios estén satisfechos.
Cada fase debe actualizar esta tabla con evidencia de pruebas y limitaciones
reales; una compilación verde por sí sola no demuestra paridad con Codex.

## Mapa del clon de Codex

Estas rutas del commit fijado guían la implementación original en Rust:

| Área | Fuente de lectura en `references/codex/codex-rs/` |
| --- | --- |
| Turno y muestreo | `core/src/session/turn.rs`, `core/src/tasks/regular.rs` |
| Plan y modo normal | `protocol/src/config_types.rs`, `collaboration-mode-templates/templates/` |
| Compactación local, remota y automática | `core/src/compact.rs`, `core/src/compact_remote_v2.rs`, `core/src/tasks/compact.rs`, `core/src/session/turn.rs` |
| Reanudar y reconstruir historial | `core/src/thread_manager.rs`, `core/src/session/rollout_reconstruction.rs`, `thread-store/src/`, `rollout/src/recorder.rs` |
| Herramientas y permisos | `core/src/tools/{registry,router,orchestrator,sandboxing}.rs`, `protocol/src/approvals.rs` |
| Protocolo para una UI externa | `app-server-protocol/src/protocol/common.rs`, `app-server-protocol/src/protocol/v2/` |
| Modelos y proveedores | `model-provider-info/src/lib.rs`, `model-provider/src/provider.rs`, `models-manager/src/` |

El motor local de Codex usa Responses como protocolo de proveedores; su
catálogo de modelos contiene atributos propios de Codex. OpenIDE mantiene
por separado los adaptadores y sus comprobaciones de compatibilidad. Copiar
solo el loop de turnos dejaría fuera el rollout, la recuperación, el
aislamiento y el flujo de aprobaciones.

Los límites de contexto para el adaptador OpenAI se comprobaron en las fichas
oficiales de [GPT-4.1](https://developers.openai.com/api/docs/models/gpt-4.1),
[GPT-4o](https://developers.openai.com/api/docs/models/gpt-4o),
[GPT-5](https://developers.openai.com/api/docs/models/gpt-5),
[GPT-5.4](https://developers.openai.com/api/docs/models/gpt-5.4) y
[GPT-6](https://developers.openai.com/api/docs/models). El presupuesto reserva
la salida máxima documentada mientras el adaptador no fije un máximo menor
por solicitud.

La aceptación con el adaptador nativo de OpenAI requiere una credencial de la
[API de OpenAI](https://developers.openai.com/api/docs/quickstart) disponible
para este proceso. El inicio de sesión de
[Codex CLI](https://learn.chatgpt.com/docs/developer-commands#codex-login)
puede usar OAuth de ChatGPT o una clave API, pero su sesión no autentica
automáticamente las solicitudes Responses del harness Rust.
