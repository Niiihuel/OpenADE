# Codex CLI como referencia para el harness Rust de OpenIDE

Revisión: 28 de septiembre de 2026. Código de Codex consultado en el commit
[`1cc7e236`](https://github.com/openai/codex/tree/1cc7e2361237ce7244430ee1d581c77f95c57ac8).
Las decisiones siguientes comparan ese código con el estado local de OpenIDE;
son una propuesta de evolución, no una dependencia nueva de Codex.

## Decisión de arquitectura

Mantener `openide-ui` como interfaz y motor visual en Rust. El candidato para
motor principal de sesiones interactivas locales es OpenCode mediante ACP,
pendiente de una prueba de contrato y de experiencia completa en Windows.
Conservar `openide-harness` como motor nativo existente para las funciones que
OpenIDE controle directamente y como alternativa cuando OpenCode no esté
disponible. El análisis del reparto de responsabilidades está en
[`opencode-acp-openchamber-assessment.md`](opencode-acp-openchamber-assessment.md).

Codex es una buena referencia para contratos de eventos, ciclo de turnos,
herramientas, permisos y recuperación. Su CLI puede seguir siendo un backend
externo seleccionable desde OpenIDE. Importar `codex-rs/core` como núcleo de
OpenIDE acoplaría el producto a su modelo de sesiones, configuración y cambios
internos.

La [documentación oficial de OpenAI sobre Codex App Server](https://learn.chatgpt.com/docs/app-server)
lo presenta como interfaz para clientes ricos: JSONL por `stdio`, hilos,
turnos, eventos y solicitudes de aprobación. También indica que el comando
`app-server` es experimental y no tiene soporte para producción. Se puede
evaluar como un adaptador opcional con versión fijada y pruebas de contrato;
el perfil CLI existente sigue siendo la ruta externa inmediata.

## Comparación comprobada

| Área | OpenIDE hoy | Referencia útil de Codex |
| --- | --- | --- |
| Bucle y streaming | `AgentRunner` ejecuta turnos, llama herramientas y emite eventos; `OpenAiCompatibleBackend` procesa SSE en Chat Completions. | [`session/turn.rs`](https://github.com/openai/codex/blob/1cc7e2361237ce7244430ee1d581c77f95c57ac8/codex-rs/core/src/session/turn.rs) separa el muestreo, la ejecución de herramientas, la compacción y la cancelación. |
| Herramientas | `LocalTools` ofrece cinco herramientas y `PermissionPolicy` resuelve Allow/Ask/Deny. | [`step_context.rs`](https://github.com/openai/codex/blob/1cc7e2361237ce7244430ee1d581c77f95c57ac8/codex-rs/core/src/session/step_context.rs) congela el catálogo y el `ToolRouter` para cada solicitud; [`router.rs`](https://github.com/openai/codex/blob/1cc7e2361237ce7244430ee1d581c77f95c57ac8/codex-rs/core/src/tools/router.rs) valida llamadas contra las herramientas anunciadas. |
| Aprobaciones y ejecución | Las llamadas de escritura/comando piden aprobación individual y la ventana registra la decisión. `run_command` corre con permisos del usuario. | [`orchestrator.rs`](https://github.com/openai/codex/blob/1cc7e2361237ce7244430ee1d581c77f95c57ac8/codex-rs/core/src/tools/orchestrator.rs) reúne aprobación, sandbox e intento de ejecución. La [documentación de seguridad](https://learn.chatgpt.com/docs/agent-approvals-security) distingue la aprobación humana del aislamiento del sistema operativo. |
| Contexto y recuperación | `ContextPolicy` cuenta caracteres y omite turnos/resultados antiguos; `SessionStore` conserva el historial, pero no hay resumen persistido ni admisión durable del turno antes del runner. | [`compact.rs`](https://github.com/openai/codex/blob/1cc7e2361237ce7244430ee1d581c77f95c57ac8/codex-rs/core/src/compact.rs) y la reconstrucción del rollout sirven como referencia para compactar y reanudar sin perder la relación entre llamadas y resultados. |
| Protocolos y extensiones | El backend nativo usa `/chat/completions`; el catálogo models.dev es metadata y MCP aún no tiene runtime. | Codex separa protocolo, catálogo y herramientas, y dispone de un cliente MCP. La [configuración oficial de MCP](https://learn.chatgpt.com/docs/extend/mcp) cubre `stdio`, HTTP y OAuth. |

La auditoría local detallada está en [`openide-harness/README.md`](../crates/openide-harness/README.md)
y en [el mapa de capacidades de Settings](settings-capability-map.md). En
particular, aprobación no equivale a sandbox: un comando aprobado en OpenIDE
puede alcanzar archivos fuera del proyecto con los permisos del usuario.

## Mejoras para el harness nativo existente

1. **Fortalecer el transporte actual.** El límite acumulado de 8 MiB para texto
   y argumentos SSE ya está implementado y probado con muchas tramas pequeñas.
   Mantener los errores explícitos y la cancelación existente.
2. **Añadir un adaptador Responses independiente.** Extender el contrato de
   `ModelBackend` con uso/tokens e identificadores de respuesta; conservar Chat
   Completions para proveedores que lo necesitan. Validar herramientas,
   secuencias de eventos y cancelación con pruebas de protocolo. Models.dev
   seguirá informando capacidades, sin activar un adaptador automáticamente.
3. **Hacer durable la admisión del turno.** Persistir un ID de turno y la
   solicitud antes de iniciar el runner; hacer idempotente el reintento y
   reconciliar eventos tras un cierre inesperado. Luego añadir checkpoints de
   compactación basados en tokens reales.
4. **Separar permiso y aislamiento.** Diseñar un ejecutor de comandos con
   límites del sistema operativo para Windows y otras plataformas. Mantener
   el permiso por herramienta como decisión visible del usuario, con políticas
   de lectura, escritura y red propias.
5. **Crear un registro de herramientas y un cliente MCP.** Capturar un
   catálogo inmutable por solicitud, evitar colisiones de nombres, aplicar
   permisos por servidor/herramienta y mostrar estado de conexión en Settings.
6. **Probar Codex como backend externo rico.** Si el perfil CLI deja de bastar,
   explorar `codex app-server` por `stdio`, fijar la versión, generar su esquema
   JSON y mapear sus eventos/aprobaciones al modelo de UI de OpenIDE. No usar
   sus APIs experimentales como formato persistente interno.

Estas mejoras no son requisito previo para probar OpenCode por ACP. Su prioridad
se decidirá según las funciones que sigan a cargo del motor nativo.

## Licencia y procedencia

El repositorio [openai/codex](https://github.com/openai/codex) se publica bajo
[Apache-2.0](https://github.com/openai/codex/blob/1cc7e2361237ce7244430ee1d581c77f95c57ac8/LICENSE)
y contiene un [NOTICE](https://github.com/openai/codex/blob/1cc7e2361237ce7244430ee1d581c77f95c57ac8/NOTICE).
Este documento adapta ideas de diseño; no incorpora código de Codex. Si en
el futuro se copia o modifica código, conservar licencia, avisos y atribución
correspondiente en la distribución de OpenIDE.
