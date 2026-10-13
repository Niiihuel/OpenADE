# Créditos de terceros

## Microsoft MXC

El aislamiento de comandos en Windows usa
[mxc-sdk](https://github.com/microsoft/mxc) fijado al commit
`ff53fda4a88463370891cd4a6a0b1aa06acae9e5`, Copyright (c) Microsoft
Corporation, bajo [licencia MIT](third_party/mxc_LICENSE.md). El código se
obtiene como dependencia Cargo; el harness de OpenIDE no deriva de MXC.

## Solar Icons

Los iconos incluidos en `apps/agents-window/assets/solar/` provienen de
[@solar-icons/static 2.3.2](https://www.npmjs.com/package/@solar-icons/static),
paquete de Saoudi Hakim publicado bajo MIT. El arte original **Solar Icon Set**
es de [480 Design](https://www.figma.com/community/file/1166831539721848736)
y está disponible bajo [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
OpenIDE incluye una selección de SVG originales de estilo **Linear** y aplica un único
color semántico al dibujarlos en su ventana Rust. No se redistribuye el catálogo
completo.

## Bootstrap Icons

Los iconos de interfaz incluidos en `apps/agents-window/assets/bootstrap/` provienen de
[bootstrap-icons 1.13.1](https://www.npmjs.com/package/bootstrap-icons), de The Bootstrap
Authors, publicado bajo MIT ([texto incluido](apps/agents-window/assets/bootstrap/LICENSE)).
OpenIDE incluye una selección de los SVG originales, sin modificar, en sus variantes de
contorno y de relleno, y les aplica un único color semántico al dibujarlos. No se
redistribuye el catálogo completo.

## Material Icon Theme

Los iconos de archivos, carpetas y lenguajes de `openide-ui` incluyen 165 SVG de
[Material Icon Theme 5.38.1](https://github.com/material-extensions/vscode-material-icon-theme/releases/tag/v5.38.1),
Copyright (c) 2025 Material Extensions, bajo
[licencia MIT](crates/openide-ui/assets/material-icon-theme/LICENSE).
Se conservan los colores y el arte originales. La revisión, integridad del
paquete y hashes de los archivos están registrados en
[NOTICE](crates/openide-ui/assets/material-icon-theme/NOTICE.md) y
[SOURCE.json](crates/openide-ui/assets/material-icon-theme/SOURCE.json).

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

## SVGL y recursos actuales de proveedores

Los SVG de Codex, OpenAI, OpenRouter, OpenCode, NVIDIA, DeepSeek, Anthropic,
Groq, Mistral y xAI en `apps/agents-window/assets/providers/svgl/` provienen de
[SVGL](https://github.com/pheralb/svgl), revisión
`f9b726cee1d1f3c697a18c880185e89d9a55bb70`, Copyright (c) 2022 Pablo Hdez,
bajo [licencia MIT](apps/agents-window/assets/providers/svgl/LICENSE).
Las máscaras derivadas de `assets/providers/icons/` adaptan el color al tema
nativo. OpenCode omite la placa de fondo y conserva el detalle interno por
opacidad. Groq también omite la placa de fondo y, junto con xAI, elimina
márgenes vacíos mediante el encuadre. Mistral mantiene su sombra por opacidad.
Los trazados originales están incluidos para auditar esas adaptaciones.
Las fuentes, hashes y cambios exactos están en
[SOURCE.json](apps/agents-window/assets/providers/SOURCE.json).

MiniMax y Z.ai **no provienen de SVGL**. Sus SVG en
`apps/agents-window/assets/providers/lobe/` se obtuvieron de
[`@lobehub/icons-static-svg` 1.95.1](https://www.npmjs.com/package/@lobehub/icons-static-svg/v/1.95.1),
Copyright (c) 2023 LobeHub, bajo
[licencia MIT](apps/agents-window/assets/providers/lobe/LICENSE).
Las derivaciones sólo normalizan dimensiones y eliminan CSS innecesario;
conservan los trazados y el encuadre de la colección.

Estos recursos identifican a los proveedores. Sus marcas pertenecen a sus
respectivos titulares; las licencias de las colecciones no implican afiliación
ni aprobación. No se redistribuye el catálogo completo ni se descarga arte
al ejecutar la aplicación.

## Repositorios de referencia

Warp y ZCode se clonaron como referencias técnicas y están excluidos del
paquete de OpenIDE. Consulta [docs/references.md](docs/references.md) para las
revisiones y el alcance de uso.

## Driver del navegador

El crate propio `openide-browser` no incorpora código de terceros. Su diseño
sigue a [agent-browser](https://github.com/vercel-labs/agent-browser),
Copyright 2025 Vercel Inc., bajo Apache-2.0: un cliente CDP propio y la
captura a partir del árbol de accesibilidad con referencias atadas al
documento. Se consultó su código como referencia y no se copió ni se tradujo
ninguna parte.

El texto de la captura accesible reproduce el formato de
`ariaSnapshot({ mode: 'ai' })` de
[Playwright 1.63.0](https://github.com/microsoft/playwright), Copyright (c)
Microsoft Corporation, bajo Apache-2.0, porque es el que describen las
herramientas del agente. `crates/openide-browser/src/snapshot.rs` reimplementa
en Rust, a partir de la lectura de su código, las reglas de ese formato: el
orden de los estados en cada línea, cuándo una cadena necesita comillas, y la
simplificación de contenedores y nombres redundantes. Las reglas de
`select_options`, del relleno de campos y de a qué control corresponde un
elemento, en `src/scripts.rs`, siguen las de Playwright del mismo modo. No se
incluye código de Playwright; [browser-driver.md](docs/browser-driver.md)
enumera las diferencias de comportamiento.

## Chromium embebido

En Linux la aplicación enlaza, para su navegador
([docs/browser-cef.md](docs/browser-cef.md)), el
[Chromium Embedded Framework](https://bitbucket.org/chromiumembedded/cef)
154.0.34, Copyright (c) Marshall A. Greenblatt, bajo licencia BSD de tres
cláusulas. Incluye Chromium 154.0.8037.98 y sus componentes; las licencias de
todos ellos están en el `CREDITS.html` que acompaña a la distribución de CEF.
Los enlaces a Rust son [cef-rs](https://github.com/tauri-apps/cef-rs) en la
revisión `2861f6991fae3ee31f877891382adcbc9b9f0953`, Copyright (c) 2023 Wu
Yuwei, bajo Apache-2.0 o MIT. No se copia código de ninguno al repositorio:
CEF se obtiene con `nix/cef.nix`.

## Editor de código y Unicode

El editor propio usa las siguientes dependencias Cargo independientes:

- [Ropey 1.6.1](https://github.com/cessen/ropey), licencia MIT:
  [texto incluido](third_party/ropey-1.6.1/LICENSE.md).
- [unicode-segmentation 1.13.3](https://github.com/unicode-rs/unicode-segmentation),
  MIT o Apache 2.0: [MIT](third_party/unicode-segmentation-1.13.3/LICENSE-MIT),
  [Apache](third_party/unicode-segmentation-1.13.3/LICENSE-APACHE).
- [unicode-width 0.2.2](https://github.com/unicode-rs/unicode-width),
  MIT o Apache 2.0: [MIT](third_party/unicode-width-0.2.2/LICENSE-MIT),
  [Apache](third_party/unicode-width-0.2.2/LICENSE-APACHE).

Zed se consultó como referencia de buffers, snapshots, historial, coordenadas
visuales y diffs. No se incorporan sus crates de editor ni sus fuentes.
[native-code-editor-design.md](docs/native-code-editor-design.md) registra los
enlaces fijados a la revisión consultada y los límites de la implementación propia.

## Vistas previas nativas de documentos

El crate propio `openide-preview` integra estas dependencias Cargo para procesar
Markdown, imágenes y PDF en Rust. Los textos de licencia se conservan sin cambios
desde los paquetes del registro, con las versiones resueltas en `Cargo.lock`:

- [Hayro 0.7.1](https://github.com/LaurenzV/hayro), Copyright (c) The Hayro Authors,
  MIT o Apache 2.0: [MIT](third_party/hayro-0.7.1/LICENSE-MIT),
  [Apache](third_party/hayro-0.7.1/LICENSE-APACHE).
- [resvg 0.48.1](https://github.com/linebender/resvg), Copyright 2017 the Resvg
  Authors, MIT o Apache 2.0: [MIT](third_party/resvg-0.48.1/LICENSE-MIT),
  [Apache](third_party/resvg-0.48.1/LICENSE-APACHE).
- [image 0.25.10](https://github.com/image-rs/image), de The image-rs Developers,
  MIT o Apache 2.0: [MIT](third_party/image-0.25.10/LICENSE-MIT),
  [Apache](third_party/image-0.25.10/LICENSE-APACHE).
- [pulldown-cmark 0.13.4](https://github.com/raphlinus/pulldown-cmark), Copyright
  2015 Google Inc. All rights reserved., MIT:
  [texto incluido](third_party/pulldown-cmark-0.13.4/LICENSE).

Hayro incluye recursos con avisos propios que también se conservan:

- Las fuentes Foxit de `hayro-interpret 0.7.0`, Copyright 2014 PDFium Authors,
  bajo [licencia BSD de tres cláusulas](third_party/hayro-interpret-0.7.0/assets/LICENSE_FOXIT).
- Los CMaps de `hayro-cmap 0.1.0`, Copyright 1990–2023 Adobe,
  bajo [licencia BSD de tres cláusulas](third_party/hayro-cmap-0.1.0/assets/LICENSE.txt).
- El perfil de color CGATS de `hayro-interpret 0.7.0`, bajo
  [CC0 1.0 Universal](third_party/hayro-interpret-0.7.0/assets/CGATS_LICENSE.txt).

Estos recursos llegan mediante las dependencias Cargo; `third_party/` conserva
sus licencias y avisos, sin duplicar los archivos de fuentes ni perfiles.

## Catálogo de modelos

`apps/agents-window/assets/model-catalog/models-dev.json` es una copia reducida
del catálogo público [models.dev](https://github.com/anomalyco/models.dev),
Copyright (c) 2025 models.dev, licencia MIT. Incluye además la ventana de
contexto que publica [Codex CLI](https://github.com/openai/codex) (Apache 2.0)
para los modelos de un plan de ChatGPT. El detalle y la forma de regenerarla
están en [su aviso](apps/agents-window/assets/model-catalog/NOTICE.md).
