# Créditos de terceros

## Microsoft MXC

El aislamiento de comandos en Windows usa
[mxc-sdk](https://github.com/microsoft/mxc) fijado al commit
`ff53fda4a88463370891cd4a6a0b1aa06acae9e5`, Copyright (c) Microsoft
Corporation, bajo [licencia MIT](third_party/mxc_LICENSE.md). El código se
obtiene como dependencia Cargo; el harness de OpenIDE no deriva de MXC.

## Models.dev

La configuración de proveedores descarga metadatos de
[Models.dev](https://github.com/anomalyco/models.dev), publicados bajo
[licencia MIT](https://github.com/anomalyco/models.dev/blob/dev/LICENSE).
OpenIDE guarda una copia local del catálogo recibido. Los datos pueden cambiar
en el origen y no sustituyen la comprobación de disponibilidad del proveedor.

## Solar Icons

Los iconos incluidos en `apps/agents-window/assets/solar/` provienen de
[@solar-icons/static 2.3.2](https://www.npmjs.com/package/@solar-icons/static),
paquete de Saoudi Hakim publicado bajo MIT. El arte original **Solar Icon Set**
es de [480 Design](https://www.figma.com/community/file/1166831539721848736)
y está disponible bajo [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
OpenIDE incluye 35 SVG originales de estilo **Linear** y aplica un único
color semántico al dibujarlos en su ventana Rust. No se redistribuye el catálogo
completo.

## GitHub Octicon

El isotipo de GitHub en `apps/agents-window/assets/octicons/github-mark.svg`
proviene de [Primer Octicons](https://github.com/primer/octicons/blob/main/icons/mark-github-16.svg),
Copyright (c) GitHub, Inc., bajo la [licencia MIT](apps/agents-window/assets/octicons/LICENSE).
Se usa exclusivamente para identificar la integración con GitHub y respeta los
colores neutros del tema.

## ChatGPT Model Selector

El selector de esfuerzo en Rust toma como referencia la geometría, el movimiento
y los efectos del proyecto [chatgpt-model-selector](https://github.com/zanwei/chatgpt-model-selector),
Copyright (c) 2026 Zanwei Guo, bajo [licencia MIT](third_party/chatgpt-model-selector_LICENSE.md).
El catálogo de proveedores y modelos de OpenIDE es una integración propia.

## Lobe Icons

Los logos de Codex, OpenAI, OpenRouter y DeepSeek en
`apps/agents-window/assets/lobe/` provienen de
[Lobe Icons](https://github.com/lobehub/lobe-icons), revisión
`329f378cbd1a88f45b60cd096b9111ce16f3ea39`, Copyright (c) 2023 LobeHub,
bajo [licencia MIT](apps/agents-window/assets/lobe/LICENSE).
Se usan para identificar proveedores; OpenIDE los tiñe con el token de texto
del tema oscuro y no incluye el catálogo completo.

## Repositorios de referencia

Warp y ZCode se clonaron como referencias técnicas y están excluidos del
paquete de OpenIDE. Consulta [docs/references.md](docs/references.md) para las
revisiones y el alcance de uso.
