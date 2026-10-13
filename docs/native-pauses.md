# Pausas por límite y continuación automática

## Qué es una pausa

Un límite de uso alcanzado, la falta de crédito, una credencial rechazada o un
proveedor que sigue fallando no terminan el turno. El runner agrega
`TurnPaused` en la frontera del modelo y el turno queda abierto en
`ModelInFlight` (o `ReadyForModel` si no se envió nada): cada herramienta
anterior tiene su resultado confirmado y la solicitud rechazada nunca se
confirmó. `ResumeEngine` lo continúa sin repetir una solicitud ni una
herramienta. `replay.pause` conserva la causa hasta la siguiente respuesta
confirmada o el evento terminal.

Un registro del proceso (`limit_gate`) recuerda cada límite cerrado por
endpoint y credencial, y por familia de modelo donde el proveedor cuenta así.
Todo agente lo consulta antes de pedir: un rechazo pausa el árbol entero una
vez. Un subagente que alcanza el límite queda en pausa, y su padre se pausa en
su siguiente frontera como `WaitingOnChild`.

La conversación muestra la tarjeta de pausa: causa, hora de renovación,
**Continuar ahora**, **Cambiar de modelo**, **Cancelar**, **Abrir cuenta** y el
texto del proveedor detrás de **Detalle**. La fila de la tarea dice «en pausa ·
03:10».

## Cuándo continúa sola

`pause::auto_resume` decide con el journal y el reloj. Devuelve `At(hora)`,
`Ask` o `Manual`; toda duda es `Manual`:

- sólo `LimitReached` y `WaitingOnChild`, con hora de renovación informada;
- nunca con el ajuste **Continuar automáticamente cuando se renueve el límite**
  apagado, ni después de cancelar la reanudación con Esc;
- nunca si la renovación está a más de 24 horas;
- un intento por pausa: un fallo no se reintenta con el reloj;
- dos rearmados automáticos por turno como máximo; la cuarta pausa del mismo
  turno espera a una persona;
- si la renovación pasó hace más de media hora (app cerrada o suspendida)
  devuelve `Ask`: la tarjeta pregunta y nadie continúa solo.

La continuación nunca abre el registro de límites: si el proveedor todavía no
renovó, el turno se pausa otra vez y espera la siguiente renovación dentro de
ese tope. Un árbol en pausa continúa por las hojas (`pause::resume_order`): cada
subagente antes que el padre que lo espera, de a uno.

## En pantalla y fuera de pantalla

La conversación seleccionada continúa con el turno propio de la ventana
(`arm_auto_resume` → `drive_pause_queue` → `start_resume`), como **Continuar
ahora**.

Una conversación raíz que no está en pantalla, y la ejecución de una
automatización, continúan fuera de pantalla bajo el mismo plan
(`arm_background_resumes`). No cambian la selección, el borrador, el modelo del
composer ni el estado de lo que se está mirando, y no ocupan el turno de la
ventana: se puede seguir enviando mensajes en la conversación visible.

- **Qué corre a la vez.** Hasta dos workers fuera de pantalla, sumando
  ejecuciones de automatizaciones y continuaciones. Un árbol usa un solo worker
  y avanza una sesión por vez. El que encuentra los dos lugares ocupados queda
  armado y arranca cuando se libera uno; pasada la media hora de gracia
  pregunta, como cualquier otra pausa. Una automatización en cola, que puede
  haber pedido una persona, toma el lugar libre antes que una continuación
  armada. Nunca hay dos turnos en una sesión: cada
  worker toma el lease de la suya, y la ventana rechaza enviar, reanudar,
  compactar o eliminar una sesión de un árbol que está continuando.
- **Con qué continúa una conversación.** `root_resume_request` en modo fuera de
  pantalla no lee el composer. Proveedor y modelo salen de
  `ModelRouteCommitted` del turno; un journal sin esa ruta no continúa solo. El
  esfuerzo no está escrito con nombre, sólo dentro del perfil de solicitud:
  `journaled_route_backend` prueba cada esfuerzo que el modelo ofrece hasta que
  la ruta resultante es exactamente la del journal. El modo es el del turno
  admitido. Permisos, reglas de comandos, hooks, instrucciones, catálogo y MCP
  se arman con los ajustes actuales y `ResumeEngine` exige que sus huellas
  coincidan con las confirmadas por el turno: si algo cambió desde la pausa, no
  continúa. Los hooks se resuelven para el workspace de la sesión.
- **Subagentes.** `child_worker_request` usa del mismo modo la ruta del journal
  de cada hijo.
- **Automatizaciones.** Continúan con `FrozenNativeAutomationAuthorityV1`, sin
  nada de los ajustes interactivos; ver
  [native-automations.md](native-automations.md).
- **Lo que decide una persona.** Un permiso, una pregunta del agente o una
  herramienta de resultado incierto detienen el worker en ese punto. No se
  responde nada por la persona: la sesión queda recuperable y se avisa.
- **Abrir la tarea mientras continúa.** Se lee como una ejecución en curso, con
  su botón de detener; el historial sigue al journal.

## Avisos

Todos apuntan a la conversación raíz, por el centro de notificaciones:

| Aviso | Cuándo |
| --- | --- |
| «Límite renovado: el turno continúa» | Arrancó la continuación. |
| «Turno completado» | El turno raíz terminó. |
| «El turno continuó y espera tu decisión» | Se detuvo en un permiso, una pregunta o una herramienta incierta. |
| «No se pudo continuar el turno en pausa: …» | Una vez por pausa, con el motivo (proveedor sin conectar, ruta cambiada, journal sin ruta, identidad que no coincide). El turno sigue en pausa para **Continuar ahora**. |
| «Límite renovado: abrí la tarea para continuar» | La raíz tiene en pantalla a uno de sus subagentes: la ventana trabaja en ese árbol y no lo continúa por detrás. |

Un turno que vuelve a pausarse por el mismo límite no avisa: espera la
siguiente renovación.

## Límites conocidos

- Fuera de pantalla no se presta la pestaña del navegador integrado. Un turno
  que se admitió con las herramientas del navegador no coincide con su catálogo
  y queda para continuar a mano, con la tarea en pantalla.
- Un subagente en pausa cuya raíz ya no está en pausa no continúa solo.
- Un subagente que la continuación lanza y que pide permiso espera en su
  sesión sin aviso propio, como ya ocurre con un árbol en pantalla cuando no
  se mira la ventana.
- La continuación se arma mientras la app está abierta; al reabrirla vale la
  regla de la media hora.
- Las pruebas usan journals reales del runner y un proveedor HTTP local que
  responde 429. No se verificó con cuentas ni límites reales.
