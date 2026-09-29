# Proveedores, modelos y catálogo propio de OpenIDE

OpenIDE consulta directamente las APIs de cada proveedor. El registro de
proveedores y el catálogo pertenecen al proyecto y se implementan en Rust. No
descargamos un catálogo agregado de terceros. Una lista remota describe modelos;
no cambia por sí misma el protocolo, los permisos ni las capacidades del
harness.

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

Los adaptadores incluidos son OpenAI Responses; OpenRouter, DeepSeek, Groq,
Mistral y xAI mediante Chat Completions; y una conexión personalizada. Otros
proveedores con uno de esos protocolos pueden declararse sin añadir un `match`
por nombre en el parser o en la selección. Un protocolo diferente sí requiere
un adaptador Rust nuevo. Las opciones particulares de una solicitud se declaran
como campos del adaptador; no se deducen de prefijos del ID del modelo.

## Listado y caché

| Proveedor | Fuente directa |
| --- | --- |
| OpenAI | [`GET /v1/models`](https://developers.openai.com/api/reference/resources/models/methods/list) |
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

Las claves se leen de variables de entorno, no del catálogo ni del historial.
El login de Codex CLI pertenece a ese perfil externo y no autentica las llamadas
de API del harness nativo. Los precios de una fuente son referencias normalizadas
a USD por millón de tokens, no gasto medido. Un presupuesto de contexto ausente
queda desconocido; no se inventa una ventana para compactación automática.

El estado restante del harness se registra en
[native-harness-roadmap.md](native-harness-roadmap.md).
