# Proveedores, modelos y harness de OpenIDE

Investigación actualizada el 28 de septiembre de 2026. **Decisión de arquitectura
prevista:** usar OpenCode como motor principal del agente mediante ACP v1 y
conservar la interfaz, el workspace y Settings de OpenIDE en Rust. El harness
propio permanece como ruta de compatibilidad durante la migración. Models.dev
sigue siendo un catálogo de datos útil, pero las opciones ejecutables de una
sesión OpenCode deben provenir del propio agente. **Esta integración ACP aún no
está implementada.**

La frontera entre ACP, la API OpenCode V2 y las funciones propias de OpenIDE
se detalla en
[`opencode-acp-openchamber-assessment.md`](opencode-acp-openchamber-assessment.md).

## Estado implementado en esta iteración

`agents-window` ya descarga `api.json` desde Rust en un worker, valida un
snapshot utilizable y guarda la última copia aceptada en
`%APPDATA%/OpenIDE/models-dev-catalog.json` en
Windows (o el directorio de datos correspondiente). Usa `ETag`, refresco horario
y botón manual; si no hay conexión conserva la copia previa. El primer inicio
sin red aún no tiene snapshot incluido en el binario. Settings muestra búsqueda,
capacidades, límite de contexto, precio informado y fecha de modelo.

La selección se guarda en `native-provider.json`, sin credenciales. Hay tres
destinos de Chat Completions definidos en Rust: OpenAI, OpenRouter y DeepSeek;
el endpoint personalizado sigue disponible. Las claves se leen de variables
de entorno. Los demás proveedores se exploran, pero no se pueden seleccionar
para ejecutar. La presencia en Models.dev y la marca `tool_call` son filtros
preliminares: falta verificar disponibilidad y compatibilidad del modelo contra
la cuenta y versión de API concretas. La selección se congela en cada objeto
de backend al iniciar un turno y cada automatización nueva guarda su pareja
proveedor/modelo; las automatizaciones antiguas usan la configuración global.
Las sesiones interactivas aún no registran esa pareja como parte de su estado
persistente. ACP, otros transportes y un almacén de secretos del sistema
operativo siguen como fases pendientes. La caché y selección descritas aquí
sirven hoy al harness Rust; no representan configuración ni disponibilidad de
modelos de una futura sesión OpenCode.

El filtro de OpenAI sólo habilita IDs revisados para herramientas y streaming de
Chat Completions; los modelos nuevos, Pro y Realtime siguen visibles en el
catálogo hasta que tengan un adaptador compatible. DeepSeek sólo habilita sus
dos IDs canónicos documentados y pide explícitamente modo sin razonamiento:
su modo de razonamiento requiere conservar y reenviar `reasoning_content` en
cada solicitud con herramientas, función pendiente del harness
([DeepSeek, Thinking Mode](https://api-docs.deepseek.com/guides/thinking_mode/)).

## Qué está comprobado

- [Models.dev](https://github.com/anomalyco/models.dev/blob/dev/README.md#api)
  publica `api.json` con proveedores y sus modelos, `models.json` con hechos
  independientes del proveedor y `catalog.json` con ambas capas. Los datos se
  mantienen como TOML en el repositorio y generan la API. Incluyen identificadores,
  modalidades, capacidades, límites, precios y estado. La fuente es comunitaria:
  una novedad aparece en OpenIDE **después** de publicarse allí y de que nuestro
  cliente refresque el catálogo; no hay promesa de actualización instantánea.
  `api.json` omite tipos especializados por defecto; `?type=all` los incluye.
- El repositorio tiene una [sincronización programada cada hora](https://github.com/anomalyco/models.dev/blob/dev/.github/workflows/sync-models.yml)
  y publica cambios desde su rama de desarrollo. La [guía de sincronización](https://github.com/anomalyco/models.dev/blob/dev/sync.md)
  muestra que no todos los proveedores/modelos se crean sin revisión; por eso
  el refresco horario local sigue la publicación upstream, no detecta por sí
  solo cualquier modelo recién anunciado por un proveedor.
- Los campos `npm` y `api` describen cómo se conecta el **AI SDK de JavaScript**
  a un proveedor, no implementan el transporte para Rust. En el harness nativo,
  un modelo nuevo puede aparecer en la lista, pero sólo se puede ejecutar si
  OpenIDE tiene adaptador de protocolo, credenciales válidas y capacidades
  compatibles. Para ACP, OpenCode anuncia sus propias opciones. Los precios del
  catálogo sirven para estimar consumo, no para garantizar la factura. Véanse la
  [definición del proveedor](https://github.com/anomalyco/models.dev/blob/dev/README.md#1-create-a-provider)
  y el [diseño de uso y costos de OpenCode](https://github.com/anomalyco/opencode/blob/dev/packages/llm/DESIGN.md#usage-and-cost).
- [OpenCode usa AI SDK y Models.dev](https://opencode.ai/docs/providers).
  Su [implementación del catálogo](https://github.com/anomalyco/opencode/blob/dev/packages/core/src/models-dev.ts)
  lee primero caché o snapshot incluido, refresca en segundo plano, admite un
  archivo alternativo y conserva un fallback offline. En el código consultado,
  el endpoint predeterminado es `models.opencode.ai/api.json`, un espejo del
  catálogo; el TTL del archivo es de cinco minutos y el ciclo de refresco de
  una hora. No hay que depender de esos detalles internos para nuestro cliente.
  La [capa de proveedores](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/provider/provider.ts)
  transforma la metadata, añade adaptadores y configuración, y filtra modelos.
- [`opencode acp`](https://opencode.ai/v2/docs/cli/acp/) se lanza como proceso
  hijo y habla JSON-RPC sobre `stdin`/`stdout` mediante ACP v1. Su servidor
  privado conserva sesiones, herramientas, permisos y selección de modelo. El
  protocolo transmite creación, listado, carga, reanudación y bifurcación de
  sesiones, replay, cancelación, texto, razonamiento, herramientas, permisos y
  uso. Anuncia los modelos habilitados y, cuando el modelo tiene variantes, el
  esfuerzo disponible. La autenticación usa las credenciales de OpenCode; el
  login se realiza con `opencode auth login`. Existe un
  [SDK oficial de ACP para Rust](https://github.com/agentclientprotocol/rust-sdk):
  su interfaz v1 es estable y la de protocolo v2 sigue en desarrollo.
- [OpenChamber](https://github.com/openchamber/openchamber/blob/main/AGENTS.md)
  usa el servidor de OpenCode mediante `@opencode/client`, no ACP. Su versión
  [desktop empaqueta el CLI compatible](https://github.com/openchamber/openchamber/blob/main/README.md).
  Es una referencia válida si luego necesitamos una API más amplia. Para el
  cliente Rust actual, ACP evita introducir un host TypeScript, administrar un
  puerto y reproducir un contrato HTTP mayor. La
  [API y el cliente HTTP v2](https://opencode.ai/v2/docs/build/client/)
  siguen en beta; sus firmas pueden cambiar antes de la versión estable.
- La separación de responsabilidades se ve también en
  [ZCode Agent](https://zcode.z.ai/en/docs/agents) y
  [Warp Agent Profiles](https://docs.warp.dev/enterprise/getting-started/getting-started-developers):
  ambos tienen agente propio y configuración de modelos, autonomía y herramientas.
  [Zed](https://zed.dev/docs/ai/external-agents) muestra la integración ACP: el
  editor muestra la conversación, mientras el agente externo normalmente conserva
  sus propias credenciales, modelos, herramientas y configuración.

## Arquitectura recomendada

| Capa | Responsable en OpenIDE | Contrato |
| --- | --- | --- |
| Workspace y UI | `agents-window` y `openide-core` | Selector de carpeta local, navegación, layout, paneles, preferencias visuales y metadatos propios de OpenIDE. La carpeta elegida se pasa a ACP al crear la sesión. |
| Motor principal previsto | OpenCode mediante adaptador ACP v1 en Rust | OpenCode es autoridad para la ejecución, transcript, herramientas, permisos, credenciales, modelos y esfuerzo de sus sesiones. OpenIDE proyecta sus eventos, sin mantener una segunda copia como fuente de verdad. |
| Catálogo | Servicio Rust de OpenIDE | Snapshot validado de Models.dev, versión/fuente/fecha, caché y última copia válida. Sirve para exploración y compatibilidad del harness propio; no reemplaza las opciones anunciadas por ACP. |
| Configuración | Settings Rust, con límites de propiedad explícitos | OpenIDE configura su UI y workspace; muestra estado y opciones del agente ACP. La autenticación y configuración efectiva de proveedores de OpenCode pertenecen a OpenCode. |
| Compatibilidad | `openide-harness` y perfiles CLI existentes | Mantienen las sesiones y automatizaciones actuales durante la migración, con sus propias credenciales y políticas. El harness Rust ejecuta sólo modelos con protocolo y capacidades verificadas. |

Mientras persista la ruta nativa, su flujo de selección debe unir
`CatalogModel`, configuración del usuario y `ProviderAdapter`: `visible`
depende del catálogo y filtros del usuario; `ejecutable` exige además adaptador,
credencial y capacidades requeridas por el harness. Un proveedor o modelo
personalizado puede agregarse sin esperar a Models.dev, con campos explícitos y
estado «sin verificar». No se debe enviar una solicitud sólo porque su ID
figure en el catálogo.

Para ACP, Settings debe mostrar instalación/estado de OpenCode y los modelos,
esfuerzos y modos que el **propio agente** anuncie para la sesión. El selector
modifica esa sesión; no reescribe silenciosamente `native-provider.json` ni
promete un esfuerzo que no esté anunciado. El login del proveedor se hace
desde OpenCode, por ejemplo con `opencode auth login`. Si luego se necesita
administrar una capacidad no cubierta por ACP desde OpenIDE, evaluar un
adaptador HTTP separado con versión fijada y pruebas de contrato, sin convertir
esa API en el modelo interno de OpenIDE.

## Actualización y precisión del catálogo

1. Incluir un snapshot conocido en la distribución y guardar el último
   `api.json` validado en caché local. Refrescar al abrir Settings si está viejo,
   periódicamente en segundo plano y mediante «Actualizar ahora»; no bloquear
   la ventana ni un turno activo por un fallo de red.
2. Descargar a archivo temporal, limitar tamaño/tiempo, validar esquema y datos
   esenciales, y publicar el nuevo snapshot de forma atómica. Ante error,
   conservar el anterior y mostrar fecha y fallo. El merge con overrides locales
   debe ser determinista; un refresh no debe sobrescribir secretos ni preferencias.
3. Calcular diferencias (nuevo, cambiado, retirado/deprecado). Conservar la
   selección guardada aunque desaparezca del catálogo y mostrarla como no
   disponible hasta que usuario o proveedor la resuelva. Fijar la metadata usada
   para un turno, especialmente límites y costo, para que un refresh no cambie
   decisiones a mitad de ejecución.
4. Para el harness nativo, filtrar por capacidades reales: el catálogo puede
   indicar `tool_call`, imágenes o razonamiento, pero el adaptador y el endpoint
   pueden admitir sólo un subconjunto. Sus controles se habilitan con la
   intersección, con un override explícito para gateways y modelos no listados.
   Para sesiones ACP, usar las opciones que OpenCode anuncie para esa sesión.

## Migración desde el proyecto actual

`agents-window` ya lee `native-provider.json` y, si aún no existe, importa
`OPENIDE_API_BASE` y `OPENIDE_MODEL` para la conexión personalizada.
[`OpenAiCompatibleBackend`](../crates/openide-harness/src/backend.rs) usa
`/chat/completions`; las claves provienen de la variable del proveedor elegido.
Los perfiles CLI son una ruta distinta. Ninguna de esas rutas implementa aún
OpenCode ACP. Evolucionar así:

1. Definir en `openide-core` una interfaz de sesión/backend y una proyección de
   eventos comunes para la UI. Registrar la correspondencia entre ID de sesión
   de OpenIDE y de OpenCode, más la carpeta elegida. Mantener los historiales
   existentes como sesiones nativas; no reenviarlos como si fueran sesiones ACP
   ni duplicar el transcript de OpenCode como fuente de verdad.
2. Integrar `opencode acp` con el SDK Rust en su protocolo v1: detectar
   ejecutable/versión, iniciarlo con el workspace, crear/listar/cargar/reanudar
   sesiones, proyectar streaming y estados, responder permisos, cancelar y
   cerrar limpiamente. Verificar reconexión y replay con una versión fijada de
   OpenCode en Windows.
3. Construir el selector de modelo, esfuerzo y modo a partir de las opciones
   anunciadas por ACP para la sesión. Mostrar instalación, compatibilidad y
   estado de autenticación sin presentar el catálogo Models.dev como garantía
   de ejecución. Probar cambio de modelo y esfuerzo, permiso y cancelación de
   un turno real antes de usar ACP como ruta predeterminada.
4. Distribuir una versión comprobada del ejecutable de OpenCode con la app o
   proporcionar una instalación guiada y verificada. La ventana debe resolver
   la ruta del ejecutable y explicar cuando falta; en esta máquina de desarrollo
   `opencode` no se detectó en `PATH` el 28 de septiembre de 2026.
5. Mantener `openide-harness` y `openide-cli` para compatibilidad mientras se
   migren sesiones y automatizaciones. Continuar corrigiendo su seguridad y
   errores, pero no duplicar en el harness propio cada proveedor, herramienta y
   regla de sesión de OpenCode. Si ACP deja una función necesaria fuera, evaluar
   la [API HTTP v2](https://opencode.ai/v2/docs/api/) con versión fijada y
   pruebas de contrato; su superficie está documentada como experimental.

El mapa local [`zcode-composer-harness.md`](zcode-composer-harness.md) describe
las brechas de la ruta nativa: admisión de turnos, persistencia, permisos,
eventos, contexto y herramientas. OpenCode asumirá estas responsabilidades
para las sesiones ACP, mientras OpenIDE conservará su UI, navegación, selector
de carpeta, metadatos propios y automatizaciones todavía no migradas. La
transición exige reconciliar estados sin presentar dos historiales o políticas
de permiso simultáneos para una misma sesión.

**Licencias consultadas:** [Models.dev MIT](https://github.com/anomalyco/models.dev/blob/dev/LICENSE),
[OpenCode MIT](https://github.com/anomalyco/opencode/blob/dev/LICENSE),
[SDK ACP Rust Apache-2.0](https://github.com/agentclientprotocol/rust-sdk/blob/main/LICENSE).
