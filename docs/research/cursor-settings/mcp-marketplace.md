# MCP y Marketplace: referencia de Cursor y adaptación nativa

Referencia: Cursor 3.17.21 instalado, mismo bundle y SHA-256 registrados en el
[inventario](README.md). REA 6.4.0 analizó cinco rangos contiguos exactos del bundle,
sin ejecutar JavaScript ni conectar servidores de Cursor. Los offsets en bytes y
hashes permiten reconstruir cada rango desde el artefacto original:
[proveniencia](mcp-marketplace-projection.json).

## Evidencia

El análisis global previo conservó cobertura parcial por agotamiento del worker;
su traza sin coincidencias para MCP no demuestra ausencia de la función. El nuevo
análisis focal parseó los cinco archivos, sin errores: 12.302 nodos AST, 49.996
bytes de texto con procedencia, 15 hallazgos. La cobertura es completa **dentro de
esos rangos elegidos**, no dentro de toda la aplicación. Quedan 2.781 fronteras
semánticas desconocidas, entre ellas dependencias y servicios externos.

- Parent de REA: `ev_85084475488cdbe618404e6827b442f7b8ad3c39edbb0cd3b35cad2131eca798`.
- Proyección resumen: `ev_304f0f685d47331eb8287366feb9d8c6d523954f95347b6f358ae2a4f2e09c8a`.
- Rango de Settings MCP: SHA-256 `c6dd1194882f6350f88d5e5f8cb53063a9f337492d62b804c93a174e377428b4`.
- Rango de biblioteca de plugins: SHA-256 `fa7a469785efaae3812320aff43cb26bae83a3dd89b9789a954431754cd898d3`.

Se inspeccionaron esos módulos mediante `inspect_analysis_view`; el bundle
portable local está en `target/cursor-settings-rea/mcp-marketplace-evidence.json`.
El código propietario de los rangos permanece fuera del código distribuido.

## Comportamientos observados en los bytes

MCP mantiene búsqueda, agrupación por estado o ámbito y orden por nombre o uso.
Separa servidores locales, del workspace, instalados por plugins y de servicios
remotos personales o de equipo. La carga, la falta de autenticación, el error y
la lista vacía tienen recorridos propios. Agregar un servidor local lleva a su
configuración; los servidores de equipo o nube remiten al servicio correspondiente.
La vista agrupa herramientas y autenticación; no confunde un registro instalado
con una conexión exitosa.

La biblioteca de plugins recibe instalados y sugeridos por separado. Filtra por
procedencia y estado, conserva búsqueda, abre el detalle de un plugin y diferencia
la exploración del marketplace de importar y administrar plugins. El estado vacío
ofrece una acción de exploración y el resultado vacío conserva el contexto de la
búsqueda. Las tarjetas reciben información de publisher e icono desde sus datos.

Son observaciones estáticas de llamadas, props y literales. No verifican flags de
una cuenta, disponibilidad remota, ranking, precios, aprobación efectiva ni el
resultado de instalar o ejecutar un plugin en Cursor.

## Contrato propio de openADE

La adaptación conserva los ámbitos y servicios reales de openADE. El inspector
MCP vive en Settings, sin duplicar estado o inventar servicios de nube. La lista permite buscar servidores; el detalle concentra su
estado, inspección, autorización y herramientas. Los esquemas completos permanecen
accesibles por herramienta y los metadatos íntegros en detalles técnicos.

La selección de herramientas es explícita, se guarda por ámbito y registro, y
se aplica al próximo turno. Consultar el catálogo no llama a una herramienta.
Cambiar selección no otorga permisos. La inspección conserva hasta 512 herramientas;
el catálogo del modelo conserva su presupuesto conjunto de 128, contando también
las herramientas locales. Un servidor incompatible queda identificado sin
impedir las herramientas locales. Véase [selección MCP](../../mcp-tool-selection.md).

Los iconos provienen del catálogo completo de SVGL fijado y disponible offline.
La coincidencia única del dominio de un endpoint permite una marca automática;
los metadatos locales requieren una web oficial única y un paquete o ejecutable
que la corrobore. Los nombres por sí solos sólo sugieren candidatos. Se puede elegir
otra marca o ninguna. Esa preferencia cosmética se guarda aparte y no altera la
huella ni los permisos del runtime. PATH y los metadatos npm/uv se leen en un
worker acotado; detectar un ejecutable no lo inicia, descarga ni habilita.

Marketplace concentra el catálogo de skills y sus instalaciones. Orígenes es un
modal de administración; Plugins y MCP tienen sus páginas en Settings. Cada control tiene
una identidad de foco independiente de la acción que despacha. La separación
entre texto y acción se verifica con párrafos medidos con su ancho real, también
cuando envuelven en ventanas angostas.

## Validación de esta etapa

La suite final de `agents-window` pasó con 2.481 pruebas, cero fallos y 179
ignoradas: 178 preexistentes y el nuevo probe local opt-in, que se ejecutó
aparte y pasó contra REA 6.4.0 real. `openide-harness` pasó 694 pruebas, con sus
13 ignoradas preexistentes. Logs locales: `target/mcp-marketplace-app-validation.log`,
`target/mcp-marketplace-harness-validation.log` y
`target/mcp-marketplace-real-rea-validation.log`.

Las capturas de árboles nativos a 360, 900 y 1.400 px están en
`target/mcp-marketplace-visual-qa`. Comprueban identidades de foco, navegación,
catálogo completo, búsqueda, espaciado, geometría y color; no sustituyen una
aceptación del escritorio GPU en Windows o macOS.

El fallo de medida de párrafos con `max_width`, que pegaba el botón a la
descripción envuelta del estado vacío, se corrigió en
[limn PR #45](https://github.com/Niiihuel/limn/pull/45), revisión fijada por esta
integración `f057ddb3aede4fa6e907deb26b25526066dd7ef8`. Pasaron 1.331 pruebas de
limn, Clippy, rustdoc y el CI del PR; no se hizo merge ni release.

El build optimizado de escritorio también terminó correctamente: cero errores,
36 warnings; log `target/mcp-marketplace-desktop-build.log`.
