# Repositorios de referencia

Los clones están en `references/` y no participan en el workspace Cargo.
Warp se obtuvo el 26 de septiembre de 2026; Codex, el 28 de septiembre.
ZCode se volvió a clonar el 29 de septiembre para la implementación de Settings.
Zed se clonó el 30 de septiembre para revisar el streaming del chat nativo.
Todos son clones de profundidad 1.

| Repositorio | Ruta local | Revisión | Licencia declarada |
| --- | --- | --- | --- |
| [Warp](https://github.com/warpdotdev/warp) | `references/warp` | `5af88f49f84e70025f9c19e13f6b9ae64b624627` | `warpui` y `warpui_core`: MIT; resto: AGPL v3 |
| [ZCode](https://github.com/zai-org/ZCode) | `references/zcode` | `29628c9acdb81b703bbd4080c207a0e7ce5e276e` | Apache 2.0, con avisos de terceros en el repositorio |
| [Codex](https://github.com/openai/codex) | `references/codex` | `33a0f766a647208b471cfbcad889c67fd324ee04` | Apache 2.0, con `NOTICE` en el repositorio |
| [Zed](https://github.com/zed-industries/zed) | `references/zed` | `d284cbbc87b55d659303106ced627c8612369827` | Los crates de agente, UI y transporte consultados: GPL-3.0-or-later |

## Uso de las referencias

### Port del streaming y herramientas de OpenIDE (2 de octubre)

Se consultó el árbol local `/home/nihuel/projects/personal/openide`, con HEAD
`a9b0053bb0150cd1b71e9f4164b2819d67b92e8d`, para adaptar el comportamiento del
chat a Rust y `openide-ui`. Dentro de
`vscode/src/vs/workbench/contrib/openideAgent/` se revisaron:

- `browser/chat/parts/openideChatToolPart.ts` y
  `common/chat/openideChatToolMeta.ts`: identidad, resumen y argumentos parciales.
- `common/chat/openideChatToolPresentation.ts`: nombres de operaciones y destinos.
- `browser/chat/openideChatActivityGroups.ts`: grupos de trabajo finalizado.
- `browser/chat/parts/openideChatTerminalPart.ts`: salida incremental y vista compacta.
- `browser/chat/parts/openideChatEditPart.ts`: cambios confirmados por archivo.
- `browser/chat/parts/openideChatMarkdownPart.ts` y sus pruebas: Markdown en curso.
- `browser/chat/openideChatPresentation.ts`: estado de vista separado del contenido.

Codex, `codex-rs/codex-api/src/sse/responses.rs`, aporta la referencia de cierre
del consumidor durante la lectura del transporte. Zed,
`crates/markdown/src/markdown.rs`, aporta la referencia de reutilización de
bloques de código al recibir texto adicional. Se usaron los clones y revisiones
indicados arriba; la implementación Rust nueva es propia, sin incorporar
fuentes de Zed. No se introdujo un runtime JavaScript para el chat.

El alcance, límites y pruebas de la adaptación están en
[native-streaming.md](native-streaming.md).

La UI propia sigue la separación conceptual vista en Warp: un árbol de
elementos, estado estable para interacciones, layout y comandos de dibujo
independientes del backend. Está implementada en `openide-ui` desde cero; el
backend eframe usa wgpu. No se incorporaron fuentes ni assets de Warp.

ZCode sirvió como referencia funcional y visual para la ventana. OpenIDE
implementa sus propios tipos, componentes y ciclo de turnos en Rust.
[native-harness-roadmap.md](native-harness-roadmap.md) registra la decisión
posterior de completar el harness nativo en Rust usando el clon de Codex como
referencia de arquitectura, ciclo de turnos, herramientas y recuperación.
[codex-rust-harness-reference.md](codex-rust-harness-reference.md) documenta
las piezas de Codex consultadas para esa implementación.

[native-streaming.md](native-streaming.md#referencia-de-zed-y-mejoras-del-30-de-septiembre)
registra el recorrido por los adaptadores, eventos semánticos, ciclo del agente,
colas y renderizado de Zed, con enlaces fijados a la revisión consultada. Las
correcciones de OpenIDE son implementaciones propias; no se copiaron fuentes ni
assets del clon. `references/zed` continúa ignorado por Git y fuera de Cargo.

[conversation-environment.md](conversation-environment.md) registra las
referencias de ZCode consultadas para el selector, entorno, dock y navegador,
junto con la integración propia de Playwright y los cambios por conversación.

Los tokens de color, tipografía y densidad se mapearon desde `DESIGN.md` y
`packages/ui/src/styles.css` de ZCode a constantes semánticas Rust de
`apps/agents-window/src/theme.rs`. La disposición de la ventana es una
implementación propia.

Este documento registra procedencia técnica. La licencia que se aplique al
código nuevo de OpenIDE aún debe elegirla su propietario.
