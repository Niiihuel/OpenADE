# Proveedores, modelos y catálogo propio de OpenIDE

OpenIDE consulta directamente las APIs de cada proveedor. El registro de
proveedores y el catálogo pertenecen al proyecto y se implementan en Rust. No
descargamos un catálogo agregado de terceros. Una lista remota describe modelos;
no cambia por sí misma el protocolo, los permisos ni las capacidades del
harness.

La UX actual expone Codex, OpenRouter, OpenCode Zen/Go y NVIDIA NIM. El registro
de otros adaptadores se conserva para compatibilidad del motor y sesiones.
Ver [conexiones y flujos de autorización](provider-connections.md).

## Registro abierto

Las definiciones incorporadas están en
[`provider-definitions.json`](../apps/agents-window/src/provider-definitions.json). Un
`provider-registry.json` versionado en el directorio de datos permite agregar
proveedores que ofrezcan uno de los protocolos implementados. Cada entrada
declara endpoint, variable de credencial, protocolo, ruta de listado y un mapa
de campos JSON. La aplicación valida el tamaño, la sintaxis, los IDs, las URLs
y los nombres reservados antes de usar una entrada. Los modelos recibidos de
la red nunca suministran endpoints de inferencia ni variables de credencial.

Por ejemplo, en el directorio de datos:

```json
{
  "version": 1,
  "providers": [{
    "id": "acme",
    "name": "Acme",
    "endpoint": "https://api.example.com/v1",
    "key_env": "ACME_API_KEY",
    "wire_api": "chat_completions",
    "model_list_path": "models",
    "catalog": {
      "entries_path": "data",
      "id_path": "id",
      "tool_call_path": "capabilities.function_calling"
    }
  }]
}
```

La clave se entrega mediante `ACME_API_KEY`; el archivo sólo conserva su
nombre. El ejemplo requiere que esa API implemente Chat Completions.

Los adaptadores incluidos son Codex y OpenAI Responses, Anthropic Messages,
OpenCode con protocolo explícito por modelo, y NVIDIA NIM; OpenRouter, DeepSeek, Groq,
Mistral y xAI mediante Chat Completions; y una conexión personalizada. Otros
proveedores con uno de esos protocolos pueden declararse sin añadir un `match`
por nombre en el parser o en la selección. Un protocolo diferente sí requiere
un adaptador Rust nuevo. Las opciones particulares de una solicitud se declaran
como campos del adaptador; no se deducen de prefijos del ID del modelo.

## Selector del composer

El picker agrupa los modelos por proveedor en cabeceras plegables con icono,
cantidad de modelos y chevron. El estado plegado se guarda en
`composer-preferences.json`. La búsqueda abre inicialmente los grupos con
coincidencias; permite plegarlos durante esa búsqueda y recupera las
preferencias habituales al limpiar el filtro.

Sólo aparecen modelos de los cuatro proveedores actuales con una conexión
disponible. Codex usa el OAuth propio de OpenIDE, con llamadas directas desde
el harness. La configuración se realiza en Settings. Un botón permanente
«Conectar proveedor» abre la vista de cuentas sin borrar el borrador.
La ruta personalizada heredada y su modelo predeterminado `gpt-4.1` dejan de
ofrecerse en el catálogo y en el selector de conversaciones nuevas.

Cada tooltip aparece al costado del menú y separa nombre/ID, descripción,
modalidades, herramientas, esfuerzos, contexto/entrada/salida, tarifas y autenticación
con divisores. `model_metadata.rs` prepara esta vista por ruta exacta cuando se
actualiza el descubrimiento; el render no consulta la red. La selección
conserva la ruta exacta y los estados de compatibilidad del motor.

Entrada y salida se presentan como filas de iconos con etiquetas cortas para
texto, imagen, audio, video y archivos. Sólo aparecen modalidades declaradas
por la fuente; una modalidad desconocida conserva su nombre sin asignarle un
icono de otra categoría. Un campo visible sin información muestra «Dato no
disponible», incluidos contexto, capacidades y tarifas.

Las fichas identifican la fuente de capacidades y límites: API del proveedor,
listado de cuenta o referencia local revisada. Un par de límites parcial se
muestra sin completar el campo ausente con otra fuente. Una capacidad ausente
queda desconocida; no equivale a una declaración negativa.

El registro actual mapea tarifas de entrada y salida para OpenRouter y xAI,
normalizadas a USD por millón de tokens. Los demás listados no tienen un mapa
de precios configurado. La UI muestra «Dato no disponible» donde faltan datos; las
tarifas son referencias del catálogo y no consumo medido de la cuenta.

Codex obtiene los modelos del listado público autorizado de Sign in with
ChatGPT, conservando `slug`, `display_name`, visibilidad y los metadatos
publicados para ese ID exacto. El selector usa los modelos visibles del listado
sin inventar un modelo predeterminado ni copiar precios o límites de otro perfil.

## Listado y caché

Las fuentes visibles actuales son el listado autorizado de ChatGPT para Codex,
OpenRouter, `/zen/v1/models` o `/zen/go/v1/models` en OpenCode y
`/v1/models` de NVIDIA NIM. El refresh actual incluye esos cuatro proveedores.
Los adaptadores compatibles anteriores mantienen las siguientes definiciones:

| Proveedor | Fuente directa |
| --- | --- |
| OpenAI | [`GET /v1/models`](https://developers.openai.com/api/reference/resources/models/methods/list) |
| Anthropic | [`GET /v1/models`](https://platform.claude.com/docs/en/api/http/models/list), paginación completa y límites/esfuerzos del ID exacto |
| OpenRouter | [API de modelos](https://openrouter.ai/docs/api/api-reference/models/get-models) y listado de cuenta para descubrimiento autenticado |
| DeepSeek | [`GET /models`](https://api-docs.deepseek.com/api/list-models/) |
| Groq | [`GET /models`](https://console.groq.com/docs/models) |
| Mistral | [`GET /models`](https://docs.mistral.ai/api/endpoint/models) |
| xAI | [`GET /language-models`](https://docs.x.ai/developers/rest-api-reference/inference/models) |

Cada fuente se consulta de manera independiente con límites de tiempo y tamaño,
sin seguir redirecciones. La respuesta JSON se interpreta con las rutas
declaradas en el registro; los campos ausentes permanecen desconocidos. Una
respuesta válida reemplaza sólo los modelos de esa fuente. El cache conserva
ETag, fecha, origen y un hash de la definición y de la credencial; al cambiar
cuenta o definición descarta de inmediato los IDs antiguos de esa fuente. No
almacena claves. Si la red falla, retiene la última lista válida y muestra el
error. El seed local contiene únicamente pares revisados y se identifica como
tal; no se presenta como una comprobación reciente del proveedor.

El listado de OpenAI ofrece IDs pero no todos los datos de esfuerzo, herramientas
o contexto. Para esos campos sólo se anuncian valores comprobados de modelos
exactos. Otros listados pueden declarar esfuerzos por modelo; el control aparece
únicamente cuando el adaptador también sabe enviarlos por su protocolo. En los
demás casos la UI deja el valor automático. No hay inferencia por `starts_with`,
familia del modelo o proveedor vecino.

## Ejecución

El harness usa una interfaz interna común para ejecutar un turno y herramientas.
Responses y Chat Completions tienen serialización y streaming propios; unificar
la interfaz Rust no implica que sus cuerpos HTTP sean idénticos. La selección
de un modelo nuevo se puede intentar explícitamente como ruta sin verificar.
Una declaración negativa de herramientas o de modalidad de texto impide
seleccionarlo para el agente. La clave, el acceso de la cuenta, el contrato de
herramientas y los límites efectivos se comprueban al llamar al proveedor; el
listado no los garantiza.

Las credenciales se resuelven desde el almacén privado de conexiones; los
proveedores por clave conservan compatibilidad con su entorno. No provienen del
catálogo ni del historial. El login propio de Codex autoriza el harness mediante
Sign in with ChatGPT; no importa credenciales del CLI. Los precios son referencias normalizadas
a USD por millón de tokens, no gasto medido. Un presupuesto de contexto ausente
queda desconocido; no se inventa una ventana para compactación automática.

Ejecución, automatizaciones, Resume, recuperación de hijos, compactación manual
y el porcentaje de contexto consultan el mismo resolvedor de límites. Primero
usa el par contexto/salida del ID exacto en el listado de la cuenta; después,
el par del catálogo con origen `OfficialApi` y una verificación registrada. Una
respuesta con sólo uno de los límites no se completa con valores de otra fuente
o revisión anterior. Cuando la API no publica ninguno, puede usarse el respaldo
revisado del mismo proveedor, ID y protocolo. Una declaración negativa explícita
de herramientas bloquea incluso un par revisado. Un modelo nuevo no necesita
estar en la matriz estática para recibir un presupuesto.

Por defecto se reserva el máximo de salida declarado. El host puede configurar
un tope menor en `native-output-limits.json` dentro de la raíz de datos de la app:

La ruta Codex del plan de ChatGPT no admite `max_output_tokens`: su UI no ofrece
este editor y la ejecución no aplica un tope configurado para ese perfil.

```json
{"version":1,"limits":[{"provider_id":"openai","model_id":"gpt-4o","wire_api":"responses","field":"max_output_tokens","tokens":4096}]}
```

Las entradas se vinculan al proveedor, ID y protocolo exactos; no se heredan por
prefijo. Responses acepta `max_output_tokens` (mínimo 16), y Chat Completions
permite elegir explícitamente `max_tokens` o `max_completion_tokens` para la
ruta documentada. El programa no deduce el parámetro por proveedor o familia.
Un archivo corrupto, protocolos cambiados, rutas duplicadas o un tope que supera
la salida máxima conocida bloquean la construcción del backend. No se modifica
la ventana de contexto. El resolvedor reserva el mismo tope que envía el backend
para ejecución, automatizaciones, Resume, hijos y compactación. El tope participa
en la huella del perfil de solicitud; su ausencia conserva la huella histórica.
El conteo de entrada Responses omite el tope de generación.

La [referencia oficial de Responses](https://developers.openai.com/api/reference/typescript/resources/beta/subresources/responses/methods/create)
describe `max_output_tokens` como límite de salida, incluidos tokens de
razonamiento. Reducirlo puede producir una respuesta incompleta: el adaptador
mantiene el rechazo existente de resultados truncados, incluidas herramientas.
Settings → Proveedores incluye un editor del tope para la ruta activa, con
selección explícita del parámetro Chat Completions, guardado validado y opción
para retirar sólo ese tope. Un borrador inválido no cambia el archivo; el editor
bloquea guardado/retiro mientras hay un turno en curso y no sobrescribe un archivo
corrupto. El reemplazo usa archivo temporal sincronizado y reemplazo atómico;
en Unix también sincroniza el directorio. La reapertura carga la ruta exacta.

`output_limit_settings_actions_persist_reopen_validate_and_reset_only_selected_route`
verifica acciones reales de la app, reapertura, presupuesto y preservación de
otra ruta. `output_limit_controls_are_reachable_and_require_an_explicit_chat_parameter`
verifica layout y scroll del framework propio a 360/620/1300 px, selección del
parámetro y deshabilitación de guardado. La aceptación HTTP
`plan_build_uncertain_effect_reopens_resumes_and_replays_checkpoint` reabre
stores después de una herramienta con resultado incierto; cambiar o retirar
el tope devuelve `ContextMismatch` sin eventos nuevos ni repetición del efecto.
Con el tope original se verifica el resultado manualmente, continúa el modelo
por SSE y se publica/reabre el checkpoint; el servidor comprueba el tope en Build,
Resume y compactación. Falta aceptación de esos controles con modelos reales,
validación de mínimos específicos por modelo y coordinación de escrituras de
configuración entre varias instancias de la app.

Los límites de OpenRouter provienen de su listado, con la salida en
`top_provider.max_completion_tokens`, y no garantizan la ventana de todos los
proveedores que su router podría elegir. Falta fijar o validar la ruta efectiva
para una aceptación de generación completa.

La prueba HTTP local
`refresh_accepts_removals_and_retains_last_valid_models_after_bad_response`
actualiza el par de límites, reabre el cache y demuestra que el mismo prompt
pasa por Chat Completions SSE con la primera ventana y falla antes de HTTP con
la segunda, más pequeña. También conserva el último par válido si falla el
refresh. Las pruebas de descubrimiento y resolución impiden heredar límites
por prefijo o entre proveedores y rechazan pares parciales o contradictorios.
La consulta opt-in `live_openrouter_catalog_exposes_exact_limits_and_reopens_cache`
verificó metadatos reales el 2026-09-29; no verifica generación ni acceso de cuenta.

El estado restante del harness se registra en
[native-harness-roadmap.md](native-harness-roadmap.md).

## Protocolo Messages

El registro admite `anthropic_messages` con autenticación Header y metadatos
acotados de versión/workspace. Catálogo y descubrimiento comparten paginación
declarativa y publicación completa. Los máximos de entrada y salida permanecen
independientes de una ventana total; esfuerzos y capacidades positivas/negativas
se extraen del ID exacto. El transporte y la recuperación son propios. Ver
[contrato, aceptación y límites](native-anthropic.md). Sin clave disponible no se
presenta como aceptación de inferencia externa.
