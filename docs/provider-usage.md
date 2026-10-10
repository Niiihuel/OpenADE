# Suscripciones y uso

La fila «Cuenta y uso» al pie de la barra lateral abre un resumen de las
cuentas conectadas. «Ver uso y proveedores» lleva a Settings → Uso, donde aparecen
Codex, OpenRouter, OpenCode Zen/Go y NVIDIA NIM, los mismos proveedores que en
el catálogo. El detalle de un proveedor conectado reutiliza esa lectura junto
a sus modelos. El resumen sigue al proveedor seleccionado sin modificar el
modelo, el borrador ni iniciar una conversación.

Todas las superficies son nativas y usan `openide-ui`: `Popover`, `ScrollArea`,
`Button`, `IconButton`, `Tooltip` y los elementos del framework. El tema,
los logotipos y la tipografía pertenecen a la aplicación. En Settings las tarjetas
pasan a dos columnas cuando hay espacio. El resumen lateral usa una fila de
48 px; su popover de 320 px se ajusta al contenido y limita a 320 px el cuerpo
desplazable. La cabecera y el acceso a Settings quedan fuera del scroll. Cada
cuenta muestra su identidad, estado o métricas y una acción con tooltip, sin
repetir su primera cuota como un segundo total ni anidar tarjetas.
En ventanas de poca altura la fila se reduce a un botón en el pie, para
mantener accesibles la cuenta de GitHub y Configuración. El popover abre primero
el proveedor seleccionado y conserva el orden del catálogo para los demás.

## Qué informa cada cuenta

- **OpenRouter:** `GET /api/v1/key` informa consumo de la clave, presupuesto
  restante y límites que devuelve el servicio. Un presupuesto con reinicio
  usa el restante publicado o el consumo del período correspondiente, nunca
  el acumulado histórico como si fuera mensual. Si no existe límite, se
  muestra consumo sin porcentaje. El presupuesto de la clave no es el saldo
  total de la cuenta; ese saldo se consulta en OpenRouter.
- **OpenCode Go:** `GET /zen/go/v1/usage` usa la misma clave y elección Go del
  catálogo. Muestra las ventanas `rolling`, `weekly` y `monthly` que devuelve
  el proveedor, sus porcentajes y `resetsAt`. No calcula límites monetarios
  a partir de precios de los modelos.
- **OpenCode Zen:** la cuenta conserva su identidad Zen. El saldo y consumo
  se consultan en el portal; las cuotas Go no se aplican a Zen.
- **Codex:** el OAuth propio usa Sign in with ChatGPT y el recurso público
  `api.openai.com/v1`. La documentación remite a ChatGPT Settings → Usage
  para el consumo del plan. La tarjeta ofrece ese acceso; no reutiliza esos
  tokens en WHAM ni infiere el nivel Plus/Pro a partir del catálogo.
- **NVIDIA NIM:** el componente muestra su conexión y acceso al portal. Un
  catálogo público no acredita una cuota o saldo de la cuenta.

Los datos ausentes no equivalen a cero. El contexto de conversación, los
tokens de una respuesta y los precios de referencia del catálogo no representan
el consumo de una suscripción.

## Sincronización

El host inicia una consulta al abrir la app, el resumen o Settings → Uso y
permite actualización manual. Actualiza cada 60 segundos con el resumen
visible, y cada cinco minutos con la barra lateral oculta. La red corre en
un worker; no bloquea el árbol de UI. Sólo hay una consulta en curso.

Un fallo temporal conserva la última medición válida y su fecha original,
con «Último dato»/«Sin actualizar». Una lectura con más de tres minutos
también muestra su antigüedad. No se reinicia una barra localmente cuando
pasa su fecha: se pide una nueva lectura al proveedor.

Los secretos se leen mediante el mecanismo existente de credenciales y
sólo se usan en headers. El worker usa destinos fijos, no sigue redirects,
acota el tamaño/tiempo de respuesta y publica errores sin cuerpos HTTP.
Los parsers publican mediciones completas o un error; una fila inválida
no deja una lectura parcial visible.

Las revisiones de credenciales permanecen en el host. Desconexión, renovación,
reemplazo de clave o cambio Zen/Go invalidan las lecturas afectadas y descartan
respuestas anteriores. Antes de publicar un resultado se vuelve a comprobar
la identidad. Los snapshots de UI no contienen secretos ni revisiones.

## Referencias revisadas

La composición toma ideas de tarjetas, jerarquía y barras discretas de
[ai-usage](https://github.com/yxhuang/ai-usage),
[Claude Dashboard](https://github.com/iftahs/claude-dashboard) y
[CodexBar](https://github.com/steipete/CodexBar); no importa su implementación
visual. También se revisó el aislamiento de datos de
[AI Usage Dashboard](https://github.com/neyham/ai-usage-dashboard) y Orca.

Contratos de datos:

- [OpenRouter: límites y consulta de la clave](https://openrouter.ai/docs/api/reference/limits).
- [OpenCode Go: implementación oficial de usage](https://github.com/anomalyco/opencode/blob/dev/packages/console/app/src/routes/zen/go/v1/usage.ts).
- [OpenCode Zen: facturación](https://opencode.ai/docs/zen/).
- [Sign in with ChatGPT: seguimiento de uso](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions#tracking-usage).
- [Sign in with ChatGPT: modelos e inferencia](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference).

## Verificación

`provider_usage.rs` prueba interpretación de presupuestos, Go/Zen, cifras
inválidas, publicación atómica, HTTP local, privacidad y retención de lecturas.
`provider_usage_app.rs` verifica navegación sin alterar el borrador y descarte
de una respuesta encolada al cambiar la cuenta. `provider_usage_ui.rs` prueba
acciones de mouse/teclado, scroll, anchuras compactas y estados sin datos.
El hover lateral se prueba con movimiento real del puntero y avance de la
transición: su fondo difiere de la sidebar y conserva el área clicable. Una
cuenta sola se comprueba sin espacio de scroll sobrante, con acciones de 32 px.

La prueba ignorada `export_provider_usage_visual_review` exporta frames
nativos a `target/provider-usage-visual-qa/` para inspección. Esos ejemplos
usan datos sintéticos identificados; no acreditan una consulta real con
una cuenta externa.

El recorrido nativo `scripts/dock-desktop-e2e.py` también abre este resumen en
un perfil temporal sin credenciales, comprueba sus límites, lo cierra con Escape
y abre Settings desde el pie. No consulta cuentas externas.

Verificación del 5 de octubre de 2026: workspace pasó 2441 pruebas, con 89
ignoradas; las 25 pruebas filtradas por `provider_usage` y la exportación nativa
también pasaron después del ajuste final. Formato y `git diff --check` pasaron.
Clippy terminó sin errores y con 104 advertencias en el árbol actual. Se
conservaron por separado los dos límites de una prueba anterior de búsqueda
web para corregir la comparación redundante que impedía Clippy.
La compilación de escritorio `cargo build -p agents-window --profile desktop`
terminó sin errores (17 advertencias). El binario está en
`target/desktop/agents-window`.

La actualización visual del 6 de octubre de 2026 pasó la suite del workspace:
2510 pruebas y 92 ignoradas. Se inspeccionaron capturas del resumen con una
cuenta, múltiples cuentas, estados de conexión y hover. Los nueve recorridos
nativos del dock, incluido abrir/cerrar este resumen y navegar a Uso, pasaron
en un escritorio Wayland aislado.
