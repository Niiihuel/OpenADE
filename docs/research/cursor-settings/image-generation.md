# Generación de imágenes en Cursor 3.17.21

Este relevamiento sigue el artefacto distribuido, sin ejecutar Cursor, consultar una cuenta, leer credenciales ni realizar solicitudes de generación o facturación. Las observaciones locales no establecen precios, cupos contratados ni derechos de una suscripción. Esos datos requieren fuentes comerciales actuales y, para una cuenta concreta, su backend autorizado.

## Artefactos y método

Se reutilizó el paquete Cursor 3.17.21 identificado en [README.md](README.md), bajo `resources/app`, y se siguió la herramienta desde el workbench hasta el ejecutor incluido en la misma instalación:

- `out/vs/workbench/workbench.glass.main.js`: SHA-256 `b6f06229dc965619ab318e3bbc9f102865101a3bc0f346630532e2558f6fc398`.
- `extensions/cursor-agent-exec/dist/main.js`: SHA-256 `d3a8d8833260c619e7f0af0d1bc59cb303e8e61548558c333014b8c93af5c4be`.

Se contrastaron las entradas de imagen del workbench Desktop y la implementación equivalente en `cursor-local-agent-runtime`, sin analizarlos completos con REA. Los offsets siguientes son **bytes UTF-8**, con origen cero; cada intervalo excluye su extremo final. Los recortes propietarios permanecen fuera del repositorio.

## Lo que presenta el cliente

En las pantallas y catálogo de Settings relevados de esta build no se identificó un módulo de generación de imágenes con selector propio de modelo, precio, plan o suscripción. Esta observación está acotada al inventario y a los dos workbenches inspeccionados; no demuestra ausencia en otras versiones, superficies o experimentos remotos.

Sí existe el comando `cursorai.action.createNewAgentWithImagePrompt` en Glass (`22814325`–`22814865`). Crea una conversación nueva en modo agente con un comienzo de prompt de imagen y la enfoca para que el usuario lo complete. No genera la imagen inmediatamente ni selecciona en ese handler un proveedor de imágenes.

La conversación representa la operación como una llamada de herramienta: estado de generación, resultado o error, prompt, archivo e imagen generada. El view model de `generateImageToolCall` (`27957638` en adelante, Glass) toma `description` de los argumentos y `imageData`/`filePath` de un resultado exitoso. La vista admite imagen inline o resolución de su archivo. Para artefactos remotos hay una resolución mediante el servicio de caché de artefactos; no es una descarga arbitraria de un proveedor elegible por el usuario en esa vista.

Los permisos del protocolo contemplan aprobación/rechazo de la solicitud de imagen, pero una rama del listener interactivo de Glass aprueba directamente `generateImageRequestQuery` con la descripción recibida (`23473455`). No se debe anunciar que cada imagen de Cursor presenta siempre un modal de consentimiento, y menos aún uno de precio, a partir de la existencia de esos mensajes de protocolo.

## Contrato de la herramienta

El esquema de argumentos del agente (`1175529`–`1175797`, Glass) contiene descripción, ruta de archivo opcional, rutas de referencias y aspect ratio. **No contiene una API key, precio, plan, cuenta ni un campo para que el agente elija el modelo de imagen.** El esquema ejecutable en la extensión (`7738296` en adelante) presenta descripción, filename opcional, referencias opcionales y los ratios `1:1`, `4:3`, `3:4`, `16:9`, `9:16`.

La herramienta se llama `GenerateImage` en varias versiones de prompt y `generate_image` en otra. Su guía exige una petición explícita de imagen del usuario, una descripción concreta y uso de herramientas de código para gráficos/tablas de datos. Esa guía orienta al modelo; no es una verificación semántica de la intención del usuario.

La implementación local añade controles observables antes de llamar al generador:

- Se incorpora al conjunto de herramientas cuando `enableGenerateImage` lo permite. Los constructores reciben esa opción del contexto; la existencia del código no prueba su valor para una cuenta.
- Rechaza ejecución si `isGenerateImageModelRestricted` está activo y su error pide cambiar el modelo seleccionado. Esa bandera viene de fuera del handler; no explica por sí misma si la restricción depende del plan, de la familia de modelo o de otra política.
- Rechaza prompts de hasta tres palabras, y también algunos de hasta cinco palabras con términos sospechosos. Es un guardrail heurístico contra llamadas accidentales, no una prueba de intención ni una cuota.
- Rechaza la ruta marcada de sólo lectura. El filtro se ejecuta antes de la inferencia y del guardado.
- Requiere un workspace/proyecto o carpeta de artefactos donde guardar el resultado. Usa los servicios de lectura/escritura de herramientas para referencias y salida, con sus resultados de permisos/error.
- Los constructores observados usan un limitador de concurrencia de dos si no reciben uno externo (`8102699`, `8402847`, extensión). Esto limita trabajos simultáneos; no representa imágenes incluidas por mes, créditos o un límite comercial.

El archivo se deriva del basename y se guarda bajo `assets` del proyecto o de la carpeta de artefactos (`7747010` en adelante, dentro del rango de herramienta). El resultado contiene ruta y datos de imagen según el destino. El handler registra resultado/latencia y, si el generador devuelve uso y hay state handler, incorpora ese uso al turno. No hay un cálculo de importe monetario en esta ruta.

## Dos fronteras que no deben confundirse

### RPC hacia el servicio de Cursor

El cliente distribuido declara `aiserver.v1.RunGenerateImageRequest` con `description`, `reference_images`, `model_id` y `max_mode` (`1768352`–`1768608`, Glass). El resultado de error declara `error` y `model_restricted` (`1768800` en adelante). El descriptor del servicio expone `RunGenerateImage` como RPC unary (`21283879`).

Estos campos prueban que el protocolo puede transportar una elección de modelo y un estado de restricción. **No prueban qué modelo se envía realmente en una cuenta, quién lo hospeda, cuánto cuesta, si una suscripción lo incluye o si acepta la API key de un proveedor particular.** La política de facturación y la implementación remota de ese RPC no forman parte del artefacto inspeccionado. Tampoco se reconstruyó aquí una cadena completa y ejecutada desde la acción de UI hasta ese RPC.

### Proveedor HTTP en la ruta local del ejecutor

La extensión contiene una segunda implementación concreta, útil para distinguir capacidad técnica de producto visible. `hde` acepta un proveedor de tipo `http`, o construye uno a partir de un `baseUrl`; `pde` toma su primer endpoint (`8217341`–`8217678`). El wiring `Xfe` habilita imágenes cuando hay un proveedor HTTP y lo permiten `enableGenerateImage` o su fallback de web tools (`8448492`–`8449692`). No es evidencia de que Settings exponga ese wiring como un selector de imágenes.

En esa ruta, el callback `Ome` hace un `POST` a `/images/generations` y usa un body creado por `Jme` (`8260907`–`8263663`):

- Modelo fijo `grok-imagine-image-quality`.
- Una imagen, resolución `1k`, ratio por defecto `1:1` y respuesta solicitada como `b64_json`.
- Prompt y aspect ratio recibidos de la herramienta; el filename sólo se utiliza como nombre del resultado.
- Referencias rechazadas explícitamente por este adaptador local, aunque existan en el contrato genérico de la herramienta.

El host HTTP recibe endpoint, credencial opcional, helper de credencial y headers de la configuración del proveedor. El helper `Qme` serializa el body y puede construir una cabecera Bearer; **se inspeccionó el código, no valores de credenciales**. El fallback a `http://127.0.0.1` del wiring no demuestra que se esté ejecutando un modelo de imágenes en localhost: habilitar la ruta sigue dependiendo de una configuración HTTP válida.

Si la respuesta aporta datos base64 se usan directamente. Si sólo aporta una URL, el fallback es configurable; su downloader restringe protocolos y rechaza loopback/redes privadas en esa ruta. El adaptador tampoco pasa una selección arbitraria de modelo de imagen desde el prompt: un servidor compatible tendría que aceptar el identificador fijo o resolverlo mediante su propia configuración. No se confirmó una UI pública para modificar ese identificador.

El helper HTTP inspeccionado no muestra que propague la señal de cancelación del turno al `fetch` de inferencia. Por ello no se infiere que cancelar visualmente un turno impida completar una solicitud ya enviada ni que elimine cualquier consumo del proveedor. El comportamiento comercial de una cancelación sigue siendo externo.

El bundle también incluye implementaciones genéricas de SDK con nombres como `gpt-image-1` y `dall-e-*` (`8344061`). Esos literales **no son un catálogo de opciones de Cursor acreditado** ni cambian el modelo fijo observado en este adaptador. Código de SDK distribuido y configuración efectiva son evidencias distintas.

## Qué permite concluir sobre cuentas y modelos propios

Hay una capacidad concreta de ejecutar imágenes a través de un proveedor HTTP configurado, además del contrato remoto de Cursor. La ruta local tiene límites reales de formato, modelo fijo y ausencia de referencias. No es suficiente para prometer que cualquier modelo de imágenes, proveedor o plan del mercado funcionará sin adaptación.

En los rangos inspeccionados no hay una tabla que transforme Free/Pro/otros planes en créditos de imagen, una factura por imagen, una comprobación de saldo ni una separación visible entre «suscripción del editor» y «crédito de API de imagen». La restricción del modelo es una entrada externa; `model_restricted`, `enableGenerateImage`, una API key disponible o un resultado de uso no permiten reconstruir esos términos comerciales.

Para una decisión de producto de openADE deben mantenerse separadas estas preguntas: quién presta la inferencia, qué formatos/modelos acepta el adaptador, quién paga al proveedor y qué remunera una suscripción de apoyo al proyecto. Esta separación es una conclusión de diseño para el proyecto, **no una política de Cursor descubierta en el binario**. Precios, condiciones, acceso de cuentas y viabilidad económica requieren investigación independiente y actualizada.

## Evidencia REA y cobertura

REA analizó siete recortes JavaScript contiguos exactos del paquete, con manifiesto de procedencia. Total: 23.505 bytes incluyendo el manifiesto, 4.579 nodos AST visitados, cero fallos de parsing y 866 fronteras semánticas desconocidas. El bootstrap de los bundles no se ejecutó.

- Análisis retenido: `ev_d60f578484918e467ef9330e90d15b45378fbd524d8e415044fd50547cb6e6e6`.
- Vista resumen: `ev_ddbc24a6588afc54dff6f4a597496e6f1c44b5cb9361742306e7b5c75376a2fc`.
- Traza del literal de endpoint: `ev_41911c4e5f5be8b2067cfccd7f0178a9db0e38763468f77bdbea5205ec61c7a5`; encuentra el endpoint dentro de la fuente seleccionada, sin acreditar ejecución o un recorrido completo de solicitudes.
- Traza `enableGenerateImage`: `ev_10e7be88bc6610628675f56e3f97a91fa5865a03a9016a2dd30aca2425b8aa71`, ambigua por el receptor de configuración externo.
- Traza `apiKey`: `ev_2c1dc9fc8fdcdfbfeb95e6f3ee35b369afe09bfb1f4a2503a9960fdb58c3c545`, ambigua por los valores y receptores externos. No contiene una credencial observada de una cuenta.

Intervalos y hashes de cada recorte:

- Extensión, herramienta: `[7737892, 7752981)`, SHA-256 `07ca859a9abfb91d9942e8b6d3e86f1f70a5c471ea877b8a066b74a2afa53226`.
- Extensión, adaptador HTTP: `[8260907, 8263663)`, SHA-256 `bd274729e31b4c9553e207f554d344fb57b40fd70d57841220e9ecac766058f4`.
- Extensión, selección HTTP: `[8217341, 8217678)`, SHA-256 `f2289c565bc41a80bcbcab7e7fe2b431891e8004392f1d9b8b0c71232fabf4a0`.
- Extensión, wiring: `[8448492, 8449692)`, SHA-256 `456bdf238eefd83127d7a126954209f63d78cbeea77a54f809afad8c7f3aef90`.
- Glass, argumentos del agente: `[1175529, 1175797)`, SHA-256 `7970f7f82e826d75fec94b1b7728bd483a6ce2caaa7876086b9e8a6021690b42`.
- Glass, request RPC: `[1768352, 1768608)`, SHA-256 `7bbef03771162cf94d970a86899be3346ad2867888a5ac923a1526b6b6743c88`.
- Glass, acción de UI: `[22814325, 22814865)`, SHA-256 `fadf41d1757344e5660be82cab3aeabf7463713be4af086344023bf46f2a3acb`.

La cobertura es completa para el parsing de esos recortes y parcial para la arquitectura de la aplicación. No cubre el backend de inferencia/facturación de Cursor, la configuración real de una cuenta, todos los experimentos remotos ni la accesibilidad de cada ruta desde la UI de esta instalación. La búsqueda negativa en Settings y un seed sin matches en una traza parcial no se utilizan como prueba de ausencia universal.
