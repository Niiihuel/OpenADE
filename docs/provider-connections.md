# Conexiones de proveedores

Settings → Proveedores y modelos ofrece Codex, OpenRouter, OpenCode Zen/Go,
NVIDIA NIM, MiniMax y GLM · Z.ai. La vista inicial usa grupos «Conectados» y «Disponibles», búsqueda
y filas de acción del framework propio `openide-ui`. Abrir un proveedor permite
conectarlo, consultar modelos, elegir uno y administrar o desconectar la cuenta.
No aparece la antigua ruta personalizada predeterminada `gpt-4.1`.

## Inicio de sesión

- **Codex:** «Continuar con ChatGPT» abre el navegador del sistema. El motor
  implementa el flujo público de
  [Sign in with ChatGPT](https://developers.openai.com/siwc/token-sharing-open-source/sign-in)
  con PKCE, state, nonce y retorno HTTP local. La primera autorización registra
  OpenIDE; las siguientes conservan el registro del mismo equipo y cuenta.
  La identidad se valida con las claves públicas del emisor antes de guardar
  la conexión. La renovación de tokens está serializada entre procesos y
  también ocurre antes de cada petición del backend. El harness ejecuta
  Responses directamente, sin lanzar el agente CLI.
- **OpenRouter:** el navegador inicia el
  [flujo OAuth PKCE](https://openrouter.ai/docs/guides/overview/auth/oauth).
  La autorización entrega una clave propia para OpenIDE; el motor verifica esa
  clave y consulta el listado de modelos de la cuenta.
- **OpenCode:** se elige Zen o Go y se ingresa la clave obtenida en su cuenta.
  Los servicios documentan conexiones por clave en
  [Zen](https://opencode.ai/docs/zen/) y [Go](https://opencode.ai/docs/go/).
  La selección conserva el endpoint del servicio y un mapa local de protocolo
  por ID exacto, basado en los endpoints publicados por OpenCode. Modelos que
  requieren un protocolo aún no implementado no se ofrecen para ejecutar.
- **NVIDIA NIM:** se ingresa la clave de NVIDIA API Catalog, siguiendo su
  [guía de conexión](https://docs.api.nvidia.com/nim/docs/api-quickstart).
  El catálogo de NIM puede ser público: recibir ese listado no certifica la
  clave ni el acceso de generación.
- **MiniMax:** clave de la API internacional, con catálogo autenticado en
  `https://api.minimax.io/v1/models`. Se verifica el listado antes de guardar;
  la disponibilidad para generar depende de la cuenta y el modelo.
- **GLM · Z.ai:** clave de la API general `https://api.z.ai/api/paas/v4`.
  Z.ai no documenta un listado de modelos por cuenta: se guarda la clave
  localmente y se ofrecen modelos revisados de su documentación, sin presentar
  ese catálogo como autorización. El acceso se comprueba con la primera
  solicitud. Esta integración corresponde a la API general, no al endpoint
  distinto de Coding Plan.

Las claves de Zen/Go y NIM se presentan como «Clave guardada». La generación y
los permisos del modelo se comprueban al hacer una solicitud. Una autorización
OAuth válida se conserva aunque luego falle el catálogo; el error permite
volver a consultar sin perder el registro.

## Credenciales

`provider_connections.rs` mantiene los secretos fuera de `ViewState`, del
catálogo y de las preferencias. Los campos de clave se enmascaran; copiar o
cortar no envía el secreto al portapapeles. Cancelar o enviar limpia el editor.
Los errores HTTP no imprimen cuerpos que puedan contener credenciales.

La raíz de datos contiene `provider-credentials/`, con permisos Unix `0700` y
archivos `0600`. Las lecturas son acotadas, rechazan enlaces y permisos abiertos;
el guardado usa temporales privados y reemplazo atómico. Estos archivos están
ignorados por Git. Esta protección depende de la cuenta del sistema operativo;
no constituye cifrado ni integración con el llavero.

OpenRouter, OpenCode, NIM, MiniMax (`MINIMAX_API_KEY`) y Z.ai (`ZAI_API_KEY`)
conservan compatibilidad con sus variables de entorno
si no existe una conexión local. Una desconexión local bloquea también esa
fuente. Codex requiere el OAuth propio de OpenIDE. Desconectarlo intenta revocar
la autorización y retira los tokens locales; si la revocación no se confirma,
la UI indica que debe retirarse el acceso desde ChatGPT.

## Modelos y plan de ChatGPT

El catálogo consulta fuentes directas de los servicios que publican un listado;
Z.ai conserva un catálogo local revisado y etiquetado como tal. Cada modelo
conserva su proveedor, ID exacto, modalidades, capacidades y límites publicados.
La lista de cuenta y la compatibilidad del transporte determinan si puede
seleccionarse. La información ausente dice «Dato no disponible» y no se
completa con la de otro proveedor o un nombre similar.

La ruta Codex usa el listado autorizado para el plan de ChatGPT y sus modelos
visibles. Su perfil de Responses envía streaming, `store: false`, mensajes de
desarrollador y herramientas del namespace `openide`. Los límites y parámetros
de ese perfil se ajustan a la
[documentación de inferencia](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference).
No ofrece topes propios de salida ni herramientas web alojadas no admitidas.

El selector de Codex consulta `GET https://api.openai.com/v1/models` con el
access token de esa cuenta, muestra únicamente `visibility: "list"` y conserva
el orden de respuesta. `display_name` es la etiqueta; `slug` es el ID ejecutado.
No hay un límite de cuatro modelos ni se agregan IDs por su aparición en otra
cuenta o en el catálogo del CLI. El orden se guarda en la caché; las copias
antiguas sin ese dato solicitan una respuesta completa para recuperarlo.
La pantalla muestra la fecha de actualización y distingue una copia guardada
de un fallo de consulta. Cambiar la selección no reordena el listado. La
actualización periódica renueva tanto los metadatos como las opciones del chat.

«Plan de ChatGPT» es un `Chip` informativo de 20 px y ancho natural, sin acción,
hover ni foco de teclado. Las acciones de cuenta mantienen sus botones propios.

La tarjeta «Cuenta y uso» de la barra lateral y Settings → Uso reúnen las
suscripciones, los presupuestos y límites disponibles del mismo catálogo.
OpenRouter y OpenCode Go admiten consulta automática; Codex, Zen y NIM ofrecen
la consulta en su portal. El detalle del proveedor reutiliza esos datos y los
errores de límite del plan orientan a Configuración de proveedores. El composer
no muestra una acción de «Uso del plan». Véase
[Suscripciones y uso](provider-usage.md) para fuentes y sincronización.

## Archivos principales

- `provider_connections.rs`: autorización, callback, validación y renovación.
- `native_provider.rs`: resolución de credenciales y protocolo del modelo.
- `model_catalog.rs`: catálogo directo y caché de metadatos.
- `settings_ui/providers.rs`: cuentas, modelos y modales de conexión.
- `crates/openide-ui/src/preferences.rs`: `PreferencesGroup` y `ActionRow`.
- `crates/openide-ui/src/tree.rs`: entrada de secretos enmascarada.
- `crates/openide-harness/src/responses.rs`: perfil del plan y token vigente.

La compilación no acredita autorización ni inferencia con una cuenta externa.
El consentimiento real se completa desde «Conectar» en la aplicación.
