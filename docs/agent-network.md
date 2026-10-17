# Red de sesiones: CLIs y harness nativo conectados

Estado: investigación y plan, 2026-10-17. Nada de esto está implementado
todavía, salvo lo que la sección [Qué hay en openADE](#qué-hay-en-openade)
marca como existente.

El objetivo es que cualquier sesión de openADE, sea del harness nativo o de una
CLI externa (Codex, Claude Code, opencode o cualquier agente ACP), pueda:

- sugerir tareas y lanzarlas como sesiones hijas;
- mandarle mensajes a otra sesión y recibir los suyos;
- avisarle a su sesión madre cuando termina, con su resultado;
- mostrarse con un único estado: necesita atención, trabajando o terminada.

## Referencia: el hub de sesiones de Claude Code Desktop

El punto de partida es una publicación de vyzual.ai en Threads (carrusel de
cuatro videos, unos 84 s en total) sobre Claude Code Desktop. Se analizó cuadro
por cuadro: un cuadro por segundo más recortes a resolución completa en los
cambios de estado. Los cuadros quedaron fuera del repositorio; abajo se
describe lo que muestran.

### Clip 1: tareas que se reparten en sesiones y reportan

1. La sesión principal termina su arreglo y dice que notó tres puntos
   pendientes. Debajo aparece una línea viva, «Suggesting task … 6s», mientras
   el modelo redacta cada sugerencia.
2. Una tarjeta «3 suggested tasks» con «Start all». Cada tarea tiene título,
   dos líneas de motivo con el archivo afectado, «Options», «Do it here»,
   «Start» y una × para descartarla.
3. Con «Start all» la tarjeta pasa a «3 started». Cada fila muestra lo que
   hace la hija en ese momento («Reading CartLineItem.tsx», «Running the cart
   tests»), un punto de actividad y un chevrón para abrirla. En la barra
   lateral las tres hijas quedan anidadas bajo la madre, que pasa a
   «Completed» mientras ellas trabajan.
4. A medida que cada hija termina, en la madre aparece una línea tenue
   «Received message from <hija>», en orden de llegada y no de inicio.
5. Con los tres mensajes, la madre arranca sola un turno nuevo: «All three
   follow-ups are done» con un enlace a cada hija. Las hijas quedan en negrita
   en la barra lateral, como no leídas.

### Clip 2: entornos guardados

- Al crear una sesión hay una lista de carpetas recientes y «Choose
  environment», con búsqueda, «Local», «Cloud», «Remote Control» y «New
  environment» (⌥⌘0).
- El diálogo «New environment» tiene cuatro pestañas:
  - **Local**: nombre, carpetas y variables de entorno. Cada carpeta lleva la
    etiqueta «Worktree» y un menú con «Select branch», «Use worktree» y
    «Remove». La rama se elige de una lista buscable: actual, `main` y ramas
    `claude/…`.
  - **Cloud**: repositorios, acceso a red limitado con dominios permitidos,
    secretos de red y script de preparación. Los secretos de red dejan que la
    sesión llame APIs sin ver los valores.
  - **SSH**: host y carpetas («elegí un host primero»).
  - **Remote Control**: instrucción de correr `claude rc` en la otra máquina.
- El pie del compositor muestra el entorno, el modo de permisos («Auto»), el
  modelo con su velocidad y esfuerzo, y un anillo de contexto.

### Clip 3: sugerir la próxima tarea mientras trabaja, y `/btw`

- La sesión ejecuta un plan de cinco pasos con tildes y spinner, y una línea
  de actividad («Editing ProductCard.tsx 18s»).
- A mitad de turno se inserta una tarjeta «New session» con la sugerencia.
  «Start» la convierte en una fila hija con su actividad en vivo, y el turno
  principal sigue sin interrumpirse.
- El compositor indica «⏎ to send, ⌘⏎ to queue for later».
- `/btw what does the 4:5 fallback do?` abre un panel a la derecha, titulado
  con la pregunta, que transmite su respuesta («1s · 342 tokens») y tiene su
  propio compositor. La sesión principal sigue trabajando al lado.

### Clip 4: una barra lateral con lo que necesita al usuario

- La barra lateral tiene tres secciones: «Needs input» (ícono de advertencia),
  «Working» y «Completed». Las sesiones se mueven solas entre ellas. Un ícono
  indica el tipo de entorno: nube o portátil.
- Al final de un turno aparece una tarjeta de cierre: un resumen y la pregunta
  «Should I open a PR for this?» con «Yes, open PR ⌘⏎» y «Not now».
- Un panel de detalles a la derecha muestra el entorno con sus carpetas y el
  interruptor de Remote Control, el contexto usado (12 %), los conectores (4) y
  las salidas, los archivos que el turno creó.

## Cómo funciona por dentro

Todo lo que sigue sale de documentación pública, del `--help` de cada CLI y
del generador oficial de esquemas de Codex. No se descompiló nada; los
términos de Anthropic no lo permiten.

### Claude Code

**Mensajes entre sesiones** ([doc](https://code.claude.com/docs/en/cross-session-messaging)):

- **Herramientas**: el modelo usa `ListAgents` para descubrir destinos y
  `SendMessage` para enviar. El mismo `SendMessage` llega a subagentes, a
  compañeros de equipo y a otras sesiones.
- **Transporte en la misma máquina**: cada sesión abre un socket Unix propio
  (su bandeja). El socket queda restringido al usuario del sistema operativo,
  y además hay un token por sesión que se exporta a los hooks y comandos hijos
  (`CLAUDE_CODE_MESSAGING_SOCKET`, `CLAUDE_CODE_MESSAGING_TOKEN`).
- **Transporte entre máquinas**: va por los servidores de Anthropic, montado
  sobre Remote Control.
- **Entrega**:
  - Con la sesión receptora ocupada, el mensaje se lee entre llamadas a
    herramientas; nunca corta una herramienta en curso.
  - Con la sesión inactiva, se arranca un turno nuevo con el mensaje. Esto es
    el «Received message from» del clip 1.
- **Aviso de inactividad**: `SendMessage` acepta `notify_when_idle`. La sesión
  vigilada manda un solo aviso cuando termina un turno sin nada en cola, o
  cuando termina. Sin respuesta, la suscripción vence a las 12 h.
- **Controles de entrada**: cada sesión decide `accept`, `hold` o `refuse`.
  Sin configuración, decide según el modo de permisos de cada lado:
  - Una sesión que pide permisos acepta mensajes y retiene los que llegan de
    una que los omite.
  - Una sesión que omite permisos retiene todo, salvo lo que llega de otra que
    también los omite.
  - Un mensaje retenido espera la aprobación del usuario, que vence a los
    5 min.
- **Seguridad**:
  - Un mensaje nunca cuenta como consentimiento del usuario y no puede
    responder un pedido de permiso.
  - No puede hacer que el receptor cambie su configuración.
  - Un `/comando` dentro del texto llega como texto y no se ejecuta.
  - Los permisos del receptor siguen aplicando.
  - Se le indica al emisor que no pida a otra sesión lo que le negaron a él.
- **Límites**:
  - Tamaño máximo cercano al millón de caracteres.
  - Se rechazan ráfagas hacia una misma sesión.
  - Se descartan repeticiones idénticas y se limita la frecuencia por emisor,
    así que un bucle entre dos sesiones se corta solo.
  - Como mucho 50 mensajes aceptados en cola y 100 retenidos.

**Desktop** ([doc](https://code.claude.com/docs/en/desktop)):

- Las tareas sugeridas se llaman «task chips». Cada una abre una sesión nueva
  con su propio worktree, en `<proyecto>/.claude/worktrees/`, y `.worktreeinclude`
  copia los archivos ignorados que hagan falta.
- `/btw` (Ctrl+;) abre un chat lateral que lee el hilo principal hasta ese
  punto, no le agrega nada y no se guarda.
- Un mensaje llega como tarjeta con el título de quien lo mandó y un enlace a
  esa sesión. Si el receptor está a mitad de tarea, el mensaje espera.

**Remote Control** ([doc](https://code.claude.com/docs/en/remote-control)):

- El proceso local solo hace pedidos HTTPS salientes: se registra en la API y
  consulta si hay trabajo, sin abrir puertos.
- Usa credenciales de corta vida, cada una con un único propósito.
- Las sesiones nuevas pueden crearse en la misma carpeta, en un worktree
  propio o como sesión única, con un máximo de 32 en simultáneo.

**Channels** ([doc](https://code.claude.com/docs/en/channels-reference)):

- Un servidor MCP que declara `experimental["claude/channel"]` puede empujar
  `notifications/claude/channel` con `{content, meta}` a una sesión abierta.
- Con `claude/channel/permission` también recibe los pedidos de permiso
  (`notifications/claude/channel/permission_request`) y puede responderlos.
- Está en vista previa de investigación y depende de una lista permitida.
- Un canal propio exige `--dangerously-load-development-channels` con
  confirmación interactiva, y no funciona con `-p`.
- No sirve como base. Queda como vía opcional para meter mensajes en una
  sesión de Claude Code abierta en terminal.

**Modo programático** ([doc](https://code.claude.com/docs/en/headless)):
`claude -p --input-format stream-json --output-format stream-json`, junto con:

- `--session-id`, `--resume` y `--fork-session`;
- `--mcp-config`;
- `--permission-prompt-tool`, que manda los pedidos de permiso a una
  herramienta MCP;
- `--permission-mode` y `--append-system-prompt`;
- `--json-schema` para un resultado estructurado.

El evento `system/init` trae una lista `capabilities` para detectar funciones
sin depender del número de versión.

### Codex

`codex app-server` habla JSON-RPC por stdio. El esquema completo sale de
`codex app-server generate-json-schema [--experimental]` (Codex es
Apache-2.0). La versión instalada (0.162.1) ofrece:

- **Hilos**: `thread/start`, `thread/resume`, `thread/fork` y
  `thread/inject_items`.
- **Turnos**:
  - `turn/start` lleva `parentTurnId` y `rootTurnId` para encadenar trabajo
    hecho en nombre de otro turno, `turnTrigger` y `outputSchema`.
  - `turn/steer` inyecta en un turno que corre y exige el `expectedTurnId`.
  - `turn/interrupt` lo corta.
- **Eventos**: `item/started|completed`, deltas de mensaje, plan y comandos,
  `turn/started|completed`, `thread/status/changed` y
  `thread/queue/changed`.
- **Pedidos del servidor al cliente**:
  `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`,
  `item/permissions/requestApproval`, `item/tool/requestUserInput` e
  `item/tool/call`. Este último ejecuta en el cliente herramientas dinámicas
  declaradas en `thread/start.dynamicTools` (experimental).
- **CLI**: `codex queue` encola un mensaje en una sesión existente,
  `codex agents` lista las sesiones del daemon local compartido, y existe
  `codex remote-control`.

### opencode y ACP

opencode 1.19.0 trae dos modos programáticos:

- `opencode acp`: servidor del Agent Client Protocol.
- `opencode serve`: servidor HTTP headless en 127.0.0.1.

ACP ([visión general](https://agentclientprotocol.com/protocol/overview)) es
JSON-RPC 2.0 con el agente como subproceso:

- Del cliente al agente: `initialize`, `session/new` (acepta servidores MCP),
  `session/load`, `session/prompt`, `session/set_mode` y `session/cancel`.
- Del agente al cliente: `session/update`, `session/request_permission`,
  `fs/*` y `terminal/*`.
- Lo único documentado a mitad de turno es `session/cancel`: un mensaje nuevo
  espera a que el prompt responda.
- Lo implementan Gemini CLI y opencode de forma nativa. Para Claude Code y
  Codex existen adaptadores.

### Conclusión técnica

El diseño de Anthropic tiene seis piezas, y todas se pueden construir en
openADE:

1. Cada sesión tiene una identidad y una bandeja de entrada.
2. Hay dos herramientas para el modelo: listar y enviar.
3. La entrega depende del estado del receptor: entre herramientas si trabaja,
   con un turno nuevo si está inactivo.
4. Hay un aviso de única vez cuando una sesión queda inactiva.
5. Hay controles de entrada y límites anti-bucle.
6. Un mensaje nunca vale como permiso.

Lo que Anthropic no necesita y openADE sí es un adaptador por agente. Cada CLI
expone una manera distinta de manejarla: `app-server`, `stream-json`, ACP o
solo una terminal.

## Qué hay en openADE

| Pieza | Estado | Dónde | Falta |
| --- | --- | --- | --- |
| Subagentes nativos como sesiones completas | Existe | `openide-harness/src/subagent_tools.rs`, `subagents.rs`, `openide-core/src/store.rs` (`create_native_child`, `SubagentLink`) | La hija no avisa a la madre: `wait_agent` consulta cada 50 ms. No hay mensajes entre hermanas ni hacia una sesión cualquiera |
| Buzón madre→hija | Existe | `SubagentCoordinator`, `QueuedSubagentInput` (16 entradas, 16 KiB) | Solo en ese sentido |
| Bandeja durable de «Redirigir» (entre herramientas) | Existe | `store.rs` `enqueue_native_steer` / `pending_native_steers`; `runner.rs` la vacía en límites seguros | Solo la usa el usuario y el host remoto |
| Cola del próximo turno | Existe | `turn_queue` en `app.rs`, `turn-queue.json` | Solo sesiones nativas |
| Servidor MCP para CLIs lanzadas por openADE | Existe | `openide-bridge` (HTTP local, token por terminal, `Approver`); `launch.rs` lo inyecta en Claude (`--mcp-config`) y Codex (`-c mcp_servers…`) | El grupo «Subagentes» está reservado y no implementado (`external_agent_settings.rs`). El token identifica la pestaña (`terminal:{tab}:{uuid}`), no la sesión. opencode no recibe el puente |
| Proxy MCP por stdio con broker | Existe | `browser_mcp.rs`, `browser_runtime.rs` (`--browser-mcp`) | Modelo a reutilizar si hace falta stdio |
| CLIs en terminal (PTY) | Existe | `start_cli_task`, `ensure_terminal_session`, `terminal.rs` | No detecta turnos, espera de respuesta ni actividad. No hay diario de eventos |
| Perfiles headless | Existe | `openide-cli` `CliRunner` (pipes, salida sin interpretar) | Sirve para automatizaciones, no para conversar |
| `codex app-server` | Parcial | `codex_discovery.rs` (`initialize`, `account/read`, `model/list`) | No ejecuta turnos |
| Hooks compatibles con Claude Code | Existe para el nativo | `compat_hooks.rs` | Nunca se aplican a CLIs externas |
| Worktrees | Existe | `environment_git.rs` (`create_worktree`) | Las hijas heredan la carpeta de la madre, sin aislamiento |
| Notificaciones con acciones | Existe | `notifications.rs` (`Notice`, tarjetas fijas, botones) | Faltan eventos de mensaje y de sesión que necesita atención |
| App remota de iOS | Existe | `openide-remote` (HTTP, emparejamiento, APNs) | No conoce la red de sesiones |

## Diseño propuesto

### Principios

1. **Una central en el proceso de escritorio.** Un registro de sesiones, una
   bandeja por sesión y una política de entrega, el mismo para todas.
   Nativas y externas tienen la misma identidad (el ID de sesión del store) y
   el mismo modelo de estado.
2. **Dos sentidos separados**:
   - **Conducción**, de openADE al agente: un adaptador por tipo de agente
     arranca turnos, recibe eventos, entrega mensajes y resuelve permisos.
   - **Coordinación**, del agente a openADE: las mismas herramientas para
     todos. En el nativo son herramientas propias; en una CLI son herramientas
     MCP del puente.
3. **Durable antes que entregado.** Un mensaje se escribe en el diario antes
   de entregarse y sobrevive a un reinicio, igual que la bandeja de
   «Redirigir».
4. **Capacidades declaradas.** Cada adaptador dice qué puede hacer:
   - eventos estructurados;
   - inyección a mitad de turno;
   - detección de inactividad;
   - permisos visibles en openADE;
   - reanudar.

   La central degrada según eso, nunca supone.
5. **Las reglas de seguridad de Anthropic, tal cual.** Están probadas y son
   las que el usuario va a esperar.

### Núcleo: `crates/openide-mesh`

Es lógica pura, sin interfaz ni red, para probarla entera con tests:

```rust
pub enum MeshStatus {
    NeedsInput(Attention), // permiso, pregunta, mensaje retenido, pausa
    Working { activity: Option<String> }, // «Leyendo CartLineItem.tsx»
    Idle { unread: bool },
    Failed,
    Offline, // adaptador caído o proceso terminado
}

pub struct Envelope {
    pub id: Uuid,
    pub from: SessionId,       // lo fija el puente, nunca el modelo
    pub to: SessionId,
    pub kind: EnvelopeKind,    // Message | Report | IdleNotice
    pub text: String,          // ≤ 64 KiB
    pub reply_to: Option<SessionId>,
    pub sender_class: PermissionClass, // Prompting | Bypass
    pub created_ms: i64,
}

pub enum Inbound { Accept, Hold, Refuse }

pub enum Delivery {
    BetweenTools,  // el receptor trabaja y su adaptador puede inyectar
    StartTurn,     // el receptor está inactivo
    Queue,         // trabaja y no puede inyectar: espera al fin del turno
    AskUser,       // retenido
    Drop(Reason),  // rechazado, repetido, ráfaga, tamaño
}
```

- **Bandeja por sesión**: `<sessions>/<id>.inbox.jsonl`, con su lock como el
  registro de subagentes. Estados: en cola, entregado, retenido, rechazado y
  vencido.
- **Límites**, con los números de Anthropic como punto de partida:
  - 50 mensajes aceptados en cola y 100 retenidos;
  - 64 KiB por mensaje;
  - ráfaga de 8 mensajes en 10 s por par emisor y receptor;
  - repetición idéntica descartada durante 30 s;
  - un mensaje retenido vence a los 5 min.
- **Suscripciones** `notify_when_idle`: un solo aviso, vencen a las 12 h, solo
  dentro de esta máquina.
- **Respuesta automática de la hija**: una hija lanzada con `report_back`
  siempre le reporta a la madre al terminar. Si no llamó a `report`, la
  central manda su respuesta final. Así «reporta» no depende de que el modelo
  se acuerde.

### Herramientas, iguales para todos los agentes

| Herramienta | Qué hace | Permiso |
| --- | --- | --- |
| `list_sessions` | Las sesiones a las que puede escribir: 20 más recientes, sin archivadas ni la propia. Muestra nombre, agente, estado, madre, entorno y actividad | Ninguno |
| `read_session` | Resumen y últimos mensajes de una sesión del mismo proyecto. Existe como `read_conversation` en el puente | Ninguno |
| `send_message` | `{to, text, notify_when_idle?}` | Ninguno; aplican los controles de entrada del receptor |
| `suggest_tasks` | `[{title, why, prompt, files?}]`: muestra la tarjeta «N tareas sugeridas». No lanza nada | Ninguno |
| `start_session` | `{title, prompt, agent?, environment?, worktree?, report_back=true}` | Pide aprobación salvo en Acceso total. Cuenta contra los límites de subagentes |
| `report` | El resultado final de una hija a su madre | Ninguno |

En el nativo, `spawn_agent`, `wait_agent`, `send_input` y `cancel_agent`
siguen como están. `start_session` es la versión de nivel sesión: elige
agente y entorno, y aparece en la barra lateral como sesión propia. El reporte
por bandeja reemplaza la consulta de `wait_agent` cuando la madre no necesita
bloquearse.

Para las CLIs, estas herramientas viven en `openide-bridge` como el grupo hoy
reservado («Subagentes», renombrado a «Sesiones»). Hay que cambiar una cosa:
el permiso que emite el puente tiene que llevar el ID de sesión de openADE, no
el de la pestaña. Así el remitente de cada mensaje no se puede falsificar.

### Adaptadores

| Adaptador | Arranque | Eventos | Inyección a mitad de turno | Inactividad | Permisos en openADE | Reanudar |
| --- | --- | --- | --- | --- | --- | --- |
| Nativo | En proceso | Diario propio | Sí, por la bandeja de «Redirigir» | `TurnCompleted` | Sí | Sí |
| Codex `app-server` | `thread/start` + `turn/start` (`parentTurnId`, `turnTrigger`) | `item/*`, `turn/*`, `thread/status/changed` | Sí, con `turn/steer` | `turn/completed` | Sí: los `requestApproval` son pedidos al cliente | `thread/resume` |
| Claude Code `stream-json` | `claude -p --input-format stream-json --output-format stream-json --session-id …` con el puente en `--mcp-config` | `assistant`, `user`, `result` y `system/*` | A verificar: mensajes escritos en stdin con el turno en curso | El evento `result` | Sí, con `--permission-prompt-tool` apuntando a una herramienta del puente | `--resume` |
| ACP (opencode, Gemini CLI, otros) | `session/new` con el puente en `mcpServers`, después `session/prompt` | `session/update` | No: el mensaje se encola hasta la respuesta del prompt | La respuesta de `session/prompt` | Sí: `session/request_permission` | `session/load` si el agente lo declara |
| Terminal (PTY, cualquier CLI) | Como hoy | Ninguno | No | Hooks: `Stop` en Claude Code; `notify` en Codex, a verificar | No: los permisos quedan en su interfaz | No |

- **Modo administrado como predeterminado.** Los tres primeros adaptadores
  externos manejan el agente por su protocolo, y openADE dibuja la
  conversación con su propia interfaz: plan con tildes, línea de actividad,
  tarjetas y permisos. Así se logra lo que muestra el video.
- **La terminal queda como opción** para quien prefiera la interfaz de la CLI.
  Ahí la red funciona a medias:
  - la CLI puede listar, enviar, lanzar y reportar a través del puente;
  - recibe mensajes solo cuando un hook avisa que quedó inactiva, pegando el
    texto con bracketed paste. Es una opción del usuario, visible, y nunca a
    mitad de turno.

### Interfaz

Es la versión en openADE de lo que muestra el video:

- **Barra lateral por estado**: «Necesita tu atención», «Trabajando» y
  «Terminadas», como una organización más junto a las actuales. Las hijas van
  anidadas bajo su madre, con el ícono del entorno y en negrita si hay algo
  sin leer.
- **Tarjeta «N tareas sugeridas»**:
  - «Iniciar todas»;
  - por tarea: «Opciones» (agente, entorno, worktree, modelo), «Hacer acá»,
    «Iniciar» y ×.

  «Hacer acá» encola la tarea en la sesión actual con la cola que ya existe.
- **Filas hijas** en la conversación de la madre, con la actividad en vivo y
  un chevrón para abrirlas.
- **Líneas «Mensaje recibido de <sesión>»**, tenues y expandibles. Después
  viene el turno que la madre arranca sola.
- **Tarjeta de cierre**: una pregunta de `ask_user` con opciones dibujadas
  como botones con atajo.
- **Panel de detalles**:
  - entorno y carpetas;
  - contexto usado (ya existe la medición);
  - conectores MCP;
  - salidas: los archivos del turno, que ya registra el seguimiento de
    actividad de archivos.
- **`/btw`**: un chat lateral efímero que lee la conversación hasta ese
  momento, usa solo herramientas de lectura, no se guarda y no agrega nada al
  hilo principal.
- **Notificaciones**: «<sesión> necesita tu atención» y «<sesión> terminó».
  Usan el centro de avisos con botones que se acaba de hacer.

### Entornos

Un entorno guardado tiene:

- nombre;
- tipo: Local al principio; SSH y otra máquina con openADE, más adelante;
- carpetas, cada una con su rama y la opción de worktree;
- variables de entorno (los secretos, en el llavero del sistema);
- un script de preparación.

Para las hijas, el worktree viene activado por defecto en repositorios Git.
Va en `<repo>/.openade/worktrees/<slug>` y usa `create_worktree`. Una lista
opcional `.worktreeinclude` copia archivos ignorados como `.env`.

### Reglas de seguridad

1. **El remitente lo fija el puente o el runner**, a partir del token o la
   sesión, nunca a partir de un argumento del modelo.
2. **Un mensaje no es consentimiento**: no responde permisos ni preguntas, no
   cambia la configuración y no ejecuta `/comandos`.
3. **El receptor sigue con sus propios permisos.** Un mensaje que pide algo
   que requiere permiso dispara el pedido de siempre.
4. **Al emisor se le indica no derivar lo que le negaron.** Si a una sesión le
   negaron algo, no se lo pide a otra; se lo pide al usuario.
5. **Los controles de entrada por sesión** usan el mismo criterio de clases de
   permiso que Anthropic. «Acceso total» cuenta como la clase que omite
   permisos.
6. **El contenido externo va marcado.** El texto de otra sesión llega como
   bloque citado y atribuido, para que el modelo no lo confunda con el
   usuario.
7. **Las automatizaciones sin nadie mirando no envían mensajes**, igual que en
   Claude Code Desktop.

## Plan por fases

Cada fase deja algo usable y probado; ninguna depende de una cuenta para sus
pruebas automáticas.

### Fase 0: confirmar los protocolos (spikes)

Verificar con las CLIs instaladas lo que la documentación no deja claro. El
resultado va a esta tabla y a pruebas manuales `#[ignore]` con cuenta real.

- Claude Code: qué pasa con un mensaje `stream-json` escrito en stdin con el
  turno en curso. También el contrato de `--permission-prompt-tool`: qué
  recibe y qué devuelve.
- Codex `app-server`: un turno completo con `turn/steer` y los pedidos de
  aprobación, con la cuenta ChatGPT.
- opencode `acp`: `session/new` con servidores MCP y
  `session/request_permission`.
- Hooks para el modo terminal: el `Stop` de Claude Code y el `notify` de
  Codex, con la forma de sus datos.

### Fase 1: núcleo `openide-mesh`

Tipos, bandeja durable, política de entrada, límites, suscripciones y
respuesta automática de la hija. Pruebas:

- bucle entre dos sesiones;
- ráfaga;
- retenido que vence;
- reinicio con mensajes en cola;
- repetición descartada;
- aviso de inactividad de una sola vez.

### Fase 2: red entre sesiones nativas

- Herramientas `list_sessions`, `send_message`, `suggest_tasks`,
  `start_session` y `report` en el harness.
- Entrega por la bandeja de «Redirigir» a mitad de turno y con turno nuevo si
  la sesión está inactiva.
- Barra lateral por estado, hijas anidadas, tarjeta de sugerencias, filas
  hijas y líneas «Mensaje recibido».

Aceptación: una prueba de extremo a extremo con proveedor guionado.

1. La madre sugiere tres tareas y el usuario toca «Iniciar todas».
2. Las hijas terminan en otro orden.
3. La madre recibe tres mensajes y hace su turno de resumen solo.
4. La barra lateral pasa por los tres estados.

### Fase 3: CLIs en terminal dentro de la red

- El grupo «Sesiones» del puente, con el permiso atado al ID de sesión.
- El puente también llega a opencode.
- Detección de inactividad con hooks, y entrega opcional al quedar inactiva.

Aceptación: Claude Code en terminal le escribe a una sesión nativa, la nativa
contesta y Claude recibe la respuesta al quedar inactivo.

### Fase 4: adaptadores administrados

En este orden:

1. Codex `app-server`: el protocolo más completo y ya en uso.
2. Claude Code `stream-json`.
3. ACP: opencode y Gemini CLI.

Cada adaptador lleva una suite de conformidad contra un agente falso (un par
JSON-RPC guionado), más una prueba real opcional. Sus eventos se proyectan al
mismo diario y a la misma interfaz que el nativo.

### Fase 5: entornos y comodidades

Entornos guardados, worktree por hija, `.worktreeinclude`, `/btw`, la tarjeta
de cierre con botones y el panel de detalles.

### Fase 6: remoto

Exponer la red al host remoto de iOS: estados, mensajes y permisos. Mensajes
entre máquinas con openADE, a través de `openide-remote`.

## Decisiones abiertas

1. **Modo por defecto de las CLIs externas**: administrado (interfaz de
   openADE) o terminal (interfaz de la CLI). Se recomienda el administrado,
   con la terminal a un clic.
2. **¿Un agente puede lanzar sesiones sin preguntar?** Se recomienda que
   sugiera por defecto, y que `start_session` pida aprobación salvo en Acceso
   total.
3. **Worktree por defecto para las hijas.** Se recomienda activarlo en
   repositorios Git.
4. **Nombre en la interfaz**: «Sesiones», «Red» o «Tareas».
