# Plan: openADE Remoto, a la altura de t3code y OpenChamber

Escrito el 2026-10-18 a pedido del usuario: mejorar la app remota (el iPhone
que maneja los agentes del PC local) mirando cómo lo resuelven t3code y
OpenChamber. Es un plan; no cambia código.

Fuentes, leídas en código y no solo en los README:

- t3code `~/projects/personal/refs/t3code` en `d8e9fd26` (2026-10-18).
- OpenChamber `~/projects/personal/refs/openchamber` en `88fc001d` (2026-10-19).
- openADE `main` en `cc8a13d` (0.4.1): `crates/openide-remote`,
  `apps/ios/OpenADERemote`, `docs/ios-remote.md`.

Las rutas de archivo de cada sección son la evidencia; lo que no se verificó en
código está marcado como *(inferido)*.

## Resumen

El remoto de openADE tiene buenas piezas sueltas (pairing de un solo uso,
tokens con hash, cursor durable, request IDs, APNs, diff/Git/archivos en el
teléfono, dictado en el dispositivo), pero está montado al revés que los otros
dos:

1. **Es otro programa.** `openide-remote` es un daemon aparte con su propio
   `AgentRunner`. La ventana de escritorio no lo arranca, no tiene pantalla de
   ajustes y no comparte sus turnos: el teléfono solo *ve* (y apenas toca) lo
   que corre en el escritorio, y el escritorio no ve lo que arrancó el teléfono
   como propio.
2. **Solo el agente nativo.** Las tareas de CLI (Claude Code, Codex, opencode)
   y las de ACP no aparecen en el teléfono.
3. **Encuesta cada segundo.** Sin stream de eventos: más latencia, más batería,
   más datos.
4. **Se configura con scripts Python** y el QR es un archivo SVG que hay que
   abrir a mano.
5. **Avisos solo de los turnos del daemon**: un turno del escritorio que pide
   permiso no llega al teléfono.

t3code y OpenChamber hacen lo mismo en lo esencial: **el servidor vive con la
app de escritorio**, la ventana de escritorio es un cliente más, todos los
clientes leen **el mismo stream de eventos** con cursor y recuperación por
snapshot, y el emparejamiento es **un QR desde Ajustes** con una lista de rutas
(LAN, túnel, relay) y una lista de dispositivos revocables.

La propuesta es mover el servidor remoto dentro de `agents-window`, cambiar la
encuesta por un stream, cubrir todos los backends y después pulir la app del
teléfono con las ideas de UX que mejor funcionan en los dos proyectos.

## Cómo lo hacen

### t3code

- **Un servidor, muchos clientes.** El binario `t3` (`apps/server`) es dueño de
  los agentes, terminales, git y SQLite. La app Electron lo arranca como proceso
  hijo (`apps/desktop/src/backend/DesktopBackendManager.ts`) y su ventana le
  habla por HTTP/WS como cualquier otro cliente. Web, escritorio y móvil
  comparten `packages/client-runtime`.
- **Transporte.** Effect RPC sobre WebSocket con JSON
  (`packages/client-runtime/src/rpc/session.ts`), métodos tipados con el permiso
  (scope) que exige cada uno (`packages/contracts/src/rpc.ts`,
  `apps/server/src/auth/RpcAuthorization.ts`). Las lecturas grandes van por HTTP
  aparte (`/threads/:id/history` paginado con cursor).
- **Sincronización.** Event sourcing en el servidor. `subscribeThread({threadId,
  afterSequence})` responde con un snapshot acotado o con los eventos que
  faltan, y un marcador `synchronized` dice "ya estás al día". Si faltan más de
  128 eventos o 1 MiB, manda un snapshot nuevo en vez de reproducir
  (`decideThreadResume` en `apps/server/src/orchestration-v2/ThreadStream.ts`).
  Cada comando lleva un `commandId` del cliente y repetirlo devuelve el mismo
  recibo. Las mutaciones nunca se reintentan solas tras reconectar.
- **Lo pesado, aparte.** Los detalles de cada tool se cortan a 32 KB en el
  stream (`WireProjection.ts`) y se piden enteros con `getTurnItem`. La salida
  de comandos es otro stream que solo se manda mientras esa fila está abierta en
  pantalla (`subscribeCommandOutput`). El texto del asistente se libera por
  bloques de markdown completos, nunca medio bloque de código
  (`assistantStreaming.ts`).
- **Reconexión.** Un supervisor por máquina con backoff de 1 s a 5 min
  (`connection/supervisor.ts`). Al volver a primer plano prueba el socket, pero
  si la app estuvo suspendida mucho tiempo lo reemplaza directamente.
  "Conectado" y "datos frescos" son dos estados distintos. El móvil guarda los
  snapshots en SQLite y se puede leer sin conexión
  (`apps/mobile/src/persistence/mobile-database.ts`).
- **Llegar al PC.** Loopback por defecto. Un interruptor "Acceso por red" lo
  publica en la LAN. Tailscale integrado (`tailscale serve --https=443` desde
  `packages/tailscale`), túnel SSH, y T3 Connect: un relay en Cloudflare con
  `cloudflared` que solo reparte credenciales, nunca ve el token.
- **Emparejar.** Código de 12 caracteres sin letras confundibles, 5 minutos, un
  solo uso (`apps/server/src/auth/PairingGrantStore.ts`), en el *fragmento* del
  link `<base>/pair#token=…` para que no quede en ningún log. QR en la terminal
  (`t3 pair`) y en Ajustes → Conexiones.
- **Rutas.** Cada máquina guardada tiene varias rutas (loopback, LAN, tailnet,
  pública, SSH, relay). Antes de mandar credenciales prueba la identidad de cada
  una en `/.well-known/t3/environment` en paralelo. Aprende las direcciones LAN y
  tailnet que le informa el servidor y cada 60 s mira si hay una ruta mejor
  (`packages/client-runtime/src/connection/routes.ts`).
- **Dispositivos.** Ajustes lista links de emparejamiento (QR, vencimiento,
  revocar) y sesiones de cliente (nombre, tipo, SO, IP, última conexión,
  "conectado" en vivo, "este", revocar, "revocar los demás")
  (`apps/web/src/components/settings/ConnectionsSettings.tsx`). Revocar cierra
  los sockets en vivo.
- **App móvil** (Expo / React Native, `apps/mobile`, aún sin publicar):
  - Lista de hilos con deslizar para archivar, borrar o posponer, y fijar.
  - Hilo con terminal nativa, archivos, revisión de diff nativa, git completo
    (commit, push, PR, ramas, worktrees) y pantallas de PR.
  - Hoja "Nueva tarea" con máquina, rama, Build/Plan y adjuntos.
  - Compositor:
    - El botón de enviar pasa a **Stop** con el borrador vacío.
    - La etiqueta cambia entre Enviar, **Encolar**, **Dirigir** y Actualizar.
    - Hay una bandeja de salida que sobrevive a reinicios.
  - Aprobaciones: tarjeta encima del compositor con "Permitir una vez",
    "Permitir en la sesión" y "Rechazar" (`PendingApprovalCard.tsx`). Gana la
    primera respuesta en el servidor; las demás reciben "ya resuelto"
    (`Orchestrator.ts:7368`).
  - Live Activity, widgets, accesos rápidos, extensión para compartir y
    hápticos.
- **Avisos.** Requieren T3 Connect: el servidor publica snapshots de actividad
  firmados y el relay manda APNs/FCM (`AgentAwarenessRelay.ts`). Hay cuatro
  disparadores, cada uno con su interruptor: espera aprobación, espera
  respuesta, terminó y falló. Avisan solo al cambiar de fase y descartan los de
  más de 2 minutos. Solo mandan títulos, nunca el contenido.
- **Feed largo** (`apps/mobile/src/features/threads/ThreadFeed.tsx`):
  - Lista con alturas fijas que abre al final y lo sigue.
  - Deja de seguir mientras arrastrás y vuelve solo en el fondo real.
  - Botón flotante "al final" y "Cargar actividad anterior".
  - Agrupación del trabajo (`apps/mobile/src/lib/threadActivity.ts`): un turno
    terminado se pliega en "Trabajó X". Las rachas de tools se juntan en una
    cabecera con resumen por tipo (leyó, editó, ejecutó), contador, marca de
    fallo y la última línea de razonamiento.

### OpenChamber

- **Servidor dentro de la app.** El servidor web Node
  (`packages/web/server/index.js`) es lo único que habla con el teléfono. La app
  Electron lo corre en su propio proceso (`startWebUiServer()` desde
  `packages/electron/main.mjs`). Sin ventana: `openchamber serve`, inicio con la
  sesión o Docker. El servidor de opencode nunca se expone; se pasa por
  `/api/*`.
- **Transporte.** Un único lector SSE de opencode
  (`lib/event-stream/global-hub.js`) reparte a los clientes por WebSocket
  (`/api/global/event/ws`), con SSE de respaldo. Las apps nativas usan solo SSE
  porque el WebSocket fallaba en Android (`packages/mobile/HANDOFF.md`). El
  cliente pasa de WS a SSE solo, con backoff de 250 ms a 5 s (60 s en segundo
  plano) (`packages/ui/src/sync/event-pipeline.ts`).
- **Recuperación.** Buffer de reproducción de 2048 eventos u 8 MiB con id por
  evento. El cliente reconecta con `lastEventId`. Si el buffer ya no lo cubre,
  recibe `ready{replayReset:true}` y vuelve a pedir el estado.
- **Menos bytes.** Junta los deltas de texto en ventanas de 50 ms sin cruzar
  otros eventos (`delta-coalescer.js`): medido, 3402 frames y 1,3 MB pasan a ser
  483 frames y 197 KB. El cliente redibuja como mucho cada 100 ms.
- **Historial por ventanas.** Unos 100 registros, ampliados hacia atrás hasta
  juntar 10 mensajes del usuario, con un máximo de 300
  (`sync/DOCUMENTATION.md`).
- **Cola de mensajes en el servidor** (`lib/message-queue`): lo que mandás
  mientras el agente trabaja sobrevive a que el teléfono se bloquee. La "atención"
  de cada sesión también vive en el servidor.
- **Llegar al PC.** Ajustes → "Agregar un dispositivo" ofrece tres opciones:
  "Solo esta computadora", "Solo la red de casa" y "Desde cualquier lugar"
  (`RemoteInstancesPage.tsx`).
  - La LAN exige contraseña (`lib/security/bind-host.js`).
  - "Desde cualquier lugar" es un relay propio con cifrado de punta a punta
    (ECDH P-256, HKDF y AES-256-GCM por dirección, `lib/relay/*`). El teléfono
    fija la clave pública del PC que viene en el QR. El relay solo mueve bytes
    cifrados.
  - También hay túneles Cloudflare y ngrok. Tailscale no tiene integración.
- **Emparejar v2** (`lib/client-auth/pairing.js`, `ui/src/lib/connectionPayload.ts`):
  - El QR es `openchamber://connect?v=2&p=<base64url JSON>` y lleva `pairingId`,
    `secret`, etiqueta, huella `ABCD-1234`, vencimiento y una **lista ordenada de
    candidatos** (LAN, túnel, relay).
  - El secreto es de un solo uso, dura 10 minutos y se guarda con hash.
  - Al canjearlo crea un dispositivo con nombre, plataforma, modelo, versión y
    última vez usado (`remote-clients.js`).
  - Contraseña, passkey y QR emiten el mismo tipo de token, revocable desde una
    sola lista.
- **Direcciones viejas.** El teléfono pide los candidatos actuales
  (`/api/client-auth/connection/candidates`) y comprueba que `/health` devuelva
  el mismo `serverId` antes de mandar su token a una dirección nueva.
- **App móvil.** Capacitor alrededor del mismo `mobile.html`, que también se
  sirve como PWA. Tiene:
  - Escáner QR que funciona sin conexión.
  - Cajón de sesiones en árbol o lista, con deslizar para archivar, borrar,
    renombrar y fijar.
  - Chat, plan, permisos y preguntas reutilizando los componentes de escritorio.
  - Cajón de trabajo: cambios (stage, commit, ramas, últimos 50 commits, PR),
    archivos, terminal PTY que se puede retomar, notas y MCP.
  - Dictado que manda el audio al PC y lo transcribe ahí (`lib/dictation`).
  - Widget, control del Centro de Control y atajo de Siri.
  - Un link nunca puede enviar un prompt: los atajos van por un plugin propio
    (`packages/mobile/README.md`).
- **Avisos.** El teléfono registra su token APNs/FCM *en su propio PC*. El PC
  firma y lo manda a su relay de push. Con una clave del teléfono, el texto
  real va cifrado y lo descifra una Notification Service Extension
  (`notifications/APNS.md`, `push-seal.js`). **No avisa al teléfono si la
  ventana de escritorio está visible** (`notifications/runtime.js`). El badge
  cuenta avisos pendientes distintos y se borra al ver o enviar.
- **UX:**
  - Tablet con barra lateral fija desde 600 px.
  - Deslizar desde los bordes: izquierda para sesiones, derecha para trabajo.
  - Teclado seguido por variable.
  - Filas de tools de una línea para lecturas y búsquedas, y desplegables para
    shell, edición y subagentes.
  - "Reconectando" deja la UI en pantalla; solo "conectando" muestra el splash.
  - Cambiar de LAN a relay reconecta sin perder el scroll.
  - Errores claros ("Acceso vencido o revocado").
  - Ajuste "Mantener la computadora despierta".
  - Registro de conexión copiable en el teléfono.

## Qué hay en openADE

Lo que ya sirve y se conserva:

- **Seguridad:** tokens de dispositivo de 64 hex con hash SHA-256 y comparación
  en tiempo constante, invitaciones de un solo uso de 10 minutos, revocación
  leída en cada pedido (`crates/openide-remote/src/auth.rs`, `pairing.rs`).
  Bind solo a loopback; la HTTPS la pone Tailscale Serve.
- **API REST tipada** (`src/api.rs`): proyectos, sesiones (lista, buscar,
  renombrar, favorito, archivar), turnos con request IDs durables, dirigir,
  cancelar un turno por nombre, decidir un permiso exacto, eventos con cursor,
  archivos (ver, buscar, editar con SHA base), Git (estado, diff, stage, commit),
  modelos, adjuntos.
- **APNs propio** (`src/push.rs`), sin relay de terceros.
- **App SwiftUI** (unas 6300 líneas): varios PCs en el Keychain, QR, compositor
  flotante, dictado en el dispositivo, cámara, modelos y esfuerzo, diff
  unificado con comentarios, archivos, Git, 37 tests XCTest.

Lo que falta (y `docs/ios-remote.md` ya lo reconoce como "la base que falta"):

- Daemon aparte: `apps/` no depende de `openide-remote`; solo hay un
  latido/archivo de control (`crates/openide-harness/src/remote_control.rs`)
  para cancelar o aprobar un turno nativo del escritorio.
- Sin CLI ni ACP: `src/desktop.rs:83` filtra `backend == "native"`.
- Encuesta: `RemoteStore.swift` pide eventos cada 1 s y el resumen cada 5 s.
- Sin avisos para los turnos del escritorio.
- Sin Ajustes, sin QR en pantalla y sin lista de dispositivos.
- El teléfono no ve las colas, las tareas que siguen fuera de pantalla ni el
  estado de las CLI que el escritorio ya conoce (`off_screen_turns`,
  `cli_status.rs`).
- No se puede compilar iOS aquí. EAS se queda sin cuota hasta el 2026-11-01, y
  el V4 tiene un arreglo de color sin compilar.

## Diseño propuesto

### 1. Un solo dueño: el servidor remoto dentro de `agents-window`

Igual que la app Electron de OpenChamber: la ventana corre el servidor HTTP en
un hilo propio con su runtime de tokio, y **los turnos que pide el teléfono son
turnos de la ventana**. Usan el mismo `start_run`, las mismas colas, los mismos
`parked_runs` y la misma bitácora. Así:

- El teléfono ve **todas** las tareas, incluidas las que siguen fuera de
  pantalla, sin un segundo `AgentRunner`.
- Lo que manda el teléfono aparece en el escritorio en vivo, y al revés.
- Desaparece el latido de 4 s y el archivo de control entre procesos.

`openide-remote` se queda como biblioteca: router, auth, pairing, push,
proyección de eventos, archivos y Git. Su `host.rs` pasa a ser un trait
`RemoteHost` que implementa la ventana. El binario `openide-remote serve` puede
quedar como modo sin ventana para un servidor, con su runner actual, pero deja
de ser el camino principal.

Riesgo principal: la ventana es egui con un bucle propio. El servidor no toca
`AgentsWindow` directamente; manda comandos por un canal que el bucle drena en
cada frame, como ya hacen `poll_parked_runs` y `poll_background_resumes`, y lee
un estado compartido (lista de sesiones y buffer de eventos) detrás de un
`Arc<Mutex>`. El servidor debe seguir respondiendo aunque la ventana esté
minimizada: el bucle de egui no corre sin repaint, así que cada comando que
llega pide `request_repaint()`.

### 2. Stream de eventos con cursor y recuperación

- `GET /v1/stream?after=<seq>` con **SSE** sobre HTTP/2. Es SSE y no WebSocket
  porque:
  - el teléfono solo recibe, y los comandos siguen siendo REST con request ID;
  - el bearer va en la cabecera, sin tickets en la URL;
  - pasa sin cambios por Tailscale Serve;
  - OpenChamber tuvo que bajar sus apps nativas a SSE.
- **Contenido:**
  - Un stream global con la lista de tareas (snapshot y luego altas, cambios y
    bajas con `sequence`) más el detalle de las tareas que el teléfono declara
    abiertas (`&watch=<id>`).
  - Esto replica `subscribeShell` y `subscribeThread` de t3code sin RPC.
- **Recuperación:**
  - Buffer de 2048 eventos u 8 MiB.
  - Si el cursor quedó fuera, sale un evento `reset` con un snapshot acotado,
    y la historia más vieja se pide por páginas.
  - Un marcador `synced` avisa que el cliente está al día.
- **Menos datos:**
  - Juntar deltas de texto cada 50 ms y cortarlos en bloques de markdown
    completos.
  - Detalles de tools cortados a 32 KB, con el resto bajo demanda.
  - La salida de comandos solo si esa fila está abierta.
- La encuesta actual queda como respaldo para clientes viejos.

### 3. Todos los backends

| Backend | Ver | Enviar | Stop | Aprobar | Modelo/modo |
|---|---|---|---|---|---|
| Nativo | sí | sí (dirigir/encolar) | sí | sí | sí |
| ACP | sí | sí | sí | sí (tarjetas ACP) | opciones del agente |
| CLI en terminal | estado y últimas líneas | no en la fase 1 | sí (SIGINT) | no | no |

Las CLI siguen en su terminal. El teléfono ve su estado (trabajando, espera,
terminó, de `cli_status.rs`) y las últimas líneas de salida en modo lectura.
Escribirles es una decisión abierta (ver abajo), porque equivale a una
terminal remota y hoy está excluida a propósito.

### 4. Emparejar y administrar desde Ajustes

Nueva página **Ajustes → Remoto**:

- **Interruptor "Permitir acceso desde el teléfono"**. Apagado, el servidor no
  escucha.
- **Rutas detectadas:** loopback; Tailscale, si `tailscale status --json`
  responde, con un botón que ejecuta `tailscale serve --bg --https=443`, como
  `packages/tailscale` de t3code; y LAN. La LAN queda **apagada** por defecto:
  sin TLS, mandaría el token en claro, y la app de iOS ya rechaza HTTP remoto.
- **QR en pantalla:** dibujado con el crate `qrcode`, que ya está en el
  workspace, como imagen de limn, con la huella del PC en texto y cuenta
  regresiva. El contenido es
  `openade-remote://pair?v=2&p=<base64url {id, secreto, etiqueta, huella,
  vence, candidatos[]}>`, con el secreto en el payload y no en una URL de
  servidor.
- **Lista de dispositivos:** nombre, modelo, versión de la app, ruta
  (Tailscale/LAN), última vez y "conectado ahora", con Revocar y "Revocar los
  demás". Revocar cierra el stream en vivo.
- **"Mantener la computadora despierta mientras haya un teléfono conectado"**,
  con un inhibidor de systemd-logind.

El teléfono:

- Guarda varias rutas por PC.
- Antes de mandar el token comprueba la identidad en `/.well-known/openade`
  (huella del PC).
- Aprende las rutas nuevas que le informa el PC.
- Al volver a primer plano, o si cambia la red, prueba la mejor ruta sin
  desmontar la pantalla.

### 5. Avisos para todo lo que corre en el PC

- Se usa el cliente APNs que ya existe, pero alimentado por los eventos de la
  ventana: espera aprobación, espera respuesta, terminó y falló, con un
  interruptor cada uno en el teléfono.
- **No se avisa al teléfono si la ventana del escritorio tiene el foco**, como
  hace OpenChamber. openADE ya sabe si la ventana tiene el foco porque lo usa
  para sus propios avisos.
- Solo títulos ("openADE necesita permiso · aurora"). Más adelante, el texto
  real cifrado con una clave del teléfono y una Notification Service Extension.
- El badge cuenta tareas que esperan algo y se borra al abrirlas.
- Las acciones "Permitir" o "Rechazar" desde la pantalla bloqueada quedan para
  después: ninguno de los dos proyectos las tiene. Es una decisión de seguridad,
  porque exige Face ID para la acción.

### 6. La app del teléfono

Por orden de valor para el que usa el teléfono:

1. **Lista de tareas como la del escritorio:** agrupadas por proyecto, con
   reloj o "espera respuesta" y no leídas. Deslizar para archivar, fijar o
   renombrar.
2. **Tarjeta de aprobación encima del compositor**, con "Permitir una vez",
   "Permitir siempre este comando" (el escritorio ya guarda la regla con
   `allow_command_always`) y "Rechazar". Gana la primera respuesta, venga del
   PC o del teléfono; la otra ve "Ya respondido en el PC".
3. **Compositor con Stop, Encolar y Dirigir:** la etiqueta cambia según haya un
   turno corriendo y un borrador. La cola vive en el PC (la de la ventana), así
   que sobrevive a que el teléfono se bloquee.
4. **El trabajo agrupado:**
   - Un turno terminado se pliega en "Trabajó 2 min · leyó 4 archivos · editó 2".
   - Durante el turno, las rachas de tools son una sola cabecera con contador y
     la última línea.
   - Lecturas y búsquedas van en filas de una línea; shell y ediciones son
     desplegables con +/− y tiempo.
   - Es la misma idea que `activity_summary` del escritorio
     (`message_actions.rs`), que ya produce "leyó 4 archivos".
5. **Estados honestos:** una píldora flotante con "Reconectando…", "Sin
   conexión, mostrando lo guardado" o "Acceso revocado". "Conectado" y "al día"
   son estados distintos. Se guarda el último snapshot de cada tarea para leer
   sin red.
6. **Historial por ventanas** anclado en mensajes del usuario: abre rápido y
   tiene "Cargar anterior". Al final del feed se sigue el final solo si no estás
   arrastrando.
7. **Nueva tarea desde el teléfono** con proyecto, agente (openADE, ACP) y
   modo Build/Plan, más acceso rápido "Nueva tarea" desde el ícono.
8. **iPad:** barra lateral fija, conversación y panel de cambios.
9. Después: Live Activity del turno en curso, widget "esperando respuesta",
   extensión para compartir que abre una tarea con el adjunto, y atajo de Siri.
   Un link nunca envía un prompt.

## Plan por fases

Cada fase se puede probar en Linux salvo lo que dice "iOS". Las partes de iOS
esperan a que vuelva la cuota de EAS (2026-11-01) o a una Mac.

### Fase 0: decisiones y limpieza (horas)

- Responder las decisiones abiertas.
- Compilar y subir el V4 pendiente cuando vuelva EAS, para partir de una base
  probada en un iPhone real.

### Fase 1: servidor en la ventana (la base)

- `openide-remote` como biblioteca con el trait `RemoteHost`. La ventana lo
  implementa sobre sus sesiones, `start_run`, `cancel_selected_run`, las
  decisiones de permiso y `parked_runs`.
- Canal de comandos drenado en el bucle de la ventana, con `request_repaint`.
- Proyección de sesiones nativas, ACP y CLI (estado).
- **Aceptación:** un test de la app crea una tarea por HTTP, la ventana la
  muestra corriendo, un permiso pedido se decide por HTTP y la tarea termina
  igual que si se hubiera escrito en el escritorio. Los tests HTTP actuales de
  `crates/openide-remote/tests/http.rs` siguen pasando contra el host nuevo.

### Fase 2: Ajustes → Remoto

- Interruptor, rutas (Tailscale detectado y botón Serve), QR, lista de
  dispositivos y revocar.
- **Aceptación:** prueba visual exportada de la página (como las de Settings) y
  un test que empareja con el payload v2 y revoca cortando el acceso.

### Fase 3: stream

- `/v1/stream` SSE con buffer, `reset`, `synced`, deltas agrupados y salida de
  comandos bajo demanda.
- **Aceptación:**
  - Un test corta la conexión a mitad de turno, reconecta con `after` y recibe
    exactamente los eventos que faltaban.
  - Con el cursor demasiado viejo recibe `reset`.
  - Medir bytes contra la encuesta en un turno real.
- **iOS:** `URLSession.bytes(for:)`, con encuesta de respaldo.

### Fase 4: avisos desde la ventana

- Eventos de la ventana hacia APNs, con supresión por foco y badge.
- **Aceptación:** test con un cliente APNs falso (ya existe la forma en
  `push.rs`).
- **iOS:** ajustes por tipo de aviso. Probar la entrega en un iPhone físico,
  que nunca se verificó.

### Fase 5: UX del teléfono (iOS)

- Puntos 1–7 de la sección 6, en ese orden.
- Tests XCTest de la proyección agrupada, de la máquina de estados de
  conexión y de las rutas.

### Fase 6: más alcance (opcional)

- Texto de los avisos cifrado de punta a punta, Live Activity, widgets,
  extensión para compartir, iPad, y CLI con entrada desde el teléfono (si se
  decide).
- Relay propio "desde cualquier lugar" solo si Tailscale queda corto. Es lo más
  caro de todo: un Worker, identidad del PC, cifrado y límites.

## Decisiones abiertas

1. **¿El servidor vive en la ventana (recomendado) o sigue como daemon?** Si
   vive en la ventana, cerrar openADE corta el remoto. Mitigación: el modo
   `serve` sin ventana para quien quiera el PC siempre disponible.
2. **¿Cómo llegar desde afuera?** Recomendado: solo Tailscale, que ya está en
   uso y no necesita servidor propio. Un relay propio (OpenChamber, t3code)
   queda para más adelante.
3. **¿LAN sin Tailscale?** Recomendado: no por defecto. Sin TLS el token viaja
   en claro, y ninguno de los dos proyectos lo permite sin contraseña.
4. **¿Escribirle a una CLI desde el teléfono?** Es una terminal remota.
   Recomendado: en la fase 1 solo estado, Stop y lectura. Más adelante, "enviar
   mensaje" pegado en la terminal cuando la CLI está esperando, que es lo que ya
   hace `pr_watch` con `wake_task`.
5. **¿Permitir o rechazar desde la notificación?** Recomendado: todavía no, y
   cuando se haga, con Face ID.
6. **¿Seguir en SwiftUI nativo o pasar a web/PWA como OpenChamber?**
   Recomendado: seguir en SwiftUI. Ya hay 6300 líneas, Keychain, dictado y
   cámara. Una PWA servida por la ventana sería un segundo cliente barato para
   Android y otros PCs, pero no reemplaza al nativo.

## Qué no se copia

- El RPC de Effect y el event sourcing completo de t3code. openADE ya tiene una
  bitácora con `sequence` por sesión, que es lo que hace falta para el cursor.
- El relay en Cloudflare con Clerk (t3code) y el relay cifrado propio
  (OpenChamber) en esta etapa.
- Terminal PTY completa en el teléfono, que está excluida a propósito en
  `docs/ios-remote.md`.

## Estado (2026-10-18)

Decisiones del usuario: reestructurar todo como t3code/OpenChamber, sin
Tailscale (el PC levanta su propio servidor), SwiftUI. Face ID quedó a criterio:
bloqueo de la app con Face ID activado por defecto, sin aprobar desde la
pantalla bloqueada.

Implementado en `wt/remote-v2`: servidor HTTPS en la ventana con certificado
propio fijado por el teléfono, emparejamiento por QR con todas las rutas, stream
SSE leído de las bitácoras, comandos ejecutados por la ventana, todos los
backends, avisos APNs, Ajustes → Remoto, UPnP opcional, y la ventana que sigue
trabajando oculta. La app del iPhone se rehízo en `wt/remote-ios`. El detalle
técnico vive en `docs/ios-remote.md`.
