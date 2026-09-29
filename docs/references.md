# Repositorios de referencia

Los clones están en `references/` y no participan en el workspace Cargo.
Warp y ZCode se obtuvieron el 26 de septiembre de 2026; Codex, el 28 de
septiembre. Todos son clones de profundidad 1.

| Repositorio | Ruta local | Revisión | Licencia declarada |
| --- | --- | --- | --- |
| [Warp](https://github.com/warpdotdev/warp) | `references/warp` | `5af88f49f84e70025f9c19e13f6b9ae64b624627` | `warpui` y `warpui_core`: MIT; resto: AGPL v3 |
| [ZCode](https://github.com/zai-org/ZCode) | `references/ZCode` | `29628c9acdb81b703bbd4080c207a0e7ce5e276e` | Apache 2.0, con avisos de terceros en el repositorio |
| [Codex](https://github.com/openai/codex) | `references/codex` | `33a0f766a647208b471cfbcad889c67fd324ee04` | Apache 2.0, con `NOTICE` en el repositorio |

## Uso de las referencias

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

Los tokens de color, tipografía y densidad se mapearon desde `DESIGN.md` y
`packages/ui/src/styles.css` de ZCode a constantes semánticas Rust de
`apps/agents-window/src/theme.rs`. La disposición de la ventana es una
implementación propia.

Este documento registra procedencia técnica. La licencia que se aplique al
código nuevo de OpenIDE aún debe elegirla su propietario.
