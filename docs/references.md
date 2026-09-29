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
[warp-visual-system.md](warp-visual-system.md) registra los mecanismos MIT,
un contrato visual propio y las comprobaciones de calidad de la ventana.
[warp-sessions-components.md](warp-sessions-components.md) detalla los componentes
MIT aplicables al dock de sesiones y sus criterios de aceptación.

El harness usa ZCode como mapa funcional. Su runtime muestra un turno que
selecciona modelo y herramientas, repite pasos modelo-herramienta y emite
eventos; sus paquetes separan contratos, adaptadores, almacenamiento y
gateway. OpenIDE implementa sus propios tipos y bucle en Rust. Consulta
[zcode-feature-map.md](zcode-feature-map.md) para ver áreas cubiertas y
pendientes.
[native-harness-roadmap.md](native-harness-roadmap.md) registra la decisión
posterior de completar el harness nativo en Rust usando el clon de Codex como
referencia de arquitectura, ciclo de turnos, herramientas y recuperación.
[zcode-sessions-deep-dive.md](zcode-sessions-deep-dive.md) detalla cada control
del dock de sesiones hasta el servicio y su estado persistido; la guía de
[componentes de sesiones](warp-sessions-components.md) describe cómo aplicar
las piezas MIT de WarpUI a esa superficie.

Los tokens de color, tipografía y densidad se mapearon desde `DESIGN.md` y
`packages/ui/src/styles.css` de ZCode a constantes semánticas Rust de
`apps/agents-window/src/theme.rs`. La disposición de la ventana es una
implementación propia; [zcode-visual-map.md](zcode-visual-map.md) registra las
fuentes y decisiones visuales.

Este documento registra procedencia técnica. La licencia que se aplique al
código nuevo de OpenIDE aún debe elegirla su propietario.
