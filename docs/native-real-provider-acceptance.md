# Aceptación opt-in con proveedores reales

## Ciclo local reproducible

`crates/openide-harness/tests/native_http_cycle.rs` prueba el recorrido sin claves ni red externa mediante un servidor HTTP loopback que entrega respuestas SSE reales al adaptador Chat Completions. Se ejecuta con:

```powershell
cargo test -p openide-harness --test native_http_cycle
```

El caso `plan_build_uncertain_effect_reopens_resumes_and_replays_checkpoint` completa Plan, cierra y reabre `SessionStore`, y pasa ese historial persistido a Build. Build recibe una llamada de herramienta por streaming; el ejecutor agrega un recibo al disco y pierde su confirmación. El test inyecta un fallo al guardar el evento terminal para representar una interrupción tras el `ToolStarted` ya sincronizado. Después de otra reapertura, `ResumeEngine` detecta `ToolOutcomeUncertain`, se niega a ejecutar la herramienta otra vez y sólo continúa cuando se registra manualmente el resultado verificado. La continuación del modelo vuelve a usar SSE. A continuación `CheckpointCoordinator` obtiene un resumen por HTTP con herramientas deshabilitadas, publica el archivo y su marcador, y el test reabre ambos stores para comprobar el replay del checkpoint. El archivo de efectos contiene exactamente un recibo antes y después de la recuperación.

El servidor tiene plazos breves y peticiones guionadas; un pedido inesperado de repetición falla el test. La interrupción inyectada es un fallo de journal controlado, no una terminación real del proceso. Este test cubre el protocolo Chat Completions, mientras que la aceptación opt-in de abajo prueba también Responses y los proveedores externos. `native_http_cycle` incluye además un caso de stream truncado que comprueba que una llamada de herramienta aparentemente completa no se ejecuta sin `[DONE]`.

## Proveedores externos

`crates/openide-harness/tests/real_provider_acceptance.rs` comprueba el transporte nativo con los adaptadores reales de OpenAI Responses y DeepSeek Chat Completions. Las pruebas están marcadas `ignored` y además exigen `OPENIDE_REAL_PROVIDER_ACCEPTANCE=1`. Si faltan la clave o el modelo de un proveedor, su prueba termina sin hacer una petición.

Configura sólo los proveedores que quieras probar. El modelo debe ser un ID con herramientas de función habilitadas en tu cuenta; no hay un modelo predeterminado para estas pruebas.

| Proveedor | Clave | Modelo |
| --- | --- | --- |
| OpenAI Responses | `OPENAI_API_KEY` | `OPENIDE_OPENAI_TEST_MODEL` |
| DeepSeek Chat Completions | `DEEPSEEK_API_KEY` | `OPENIDE_DEEPSEEK_TEST_MODEL` |

En PowerShell, después de configurar las variables en la sesión actual, ejecuta:

```powershell
$env:OPENIDE_REAL_PROVIDER_ACCEPTANCE = "1"
cargo test -p openide-harness --test real_provider_acceptance -- --ignored --test-threads=1
```

Cada proveedor crea un workspace temporal con un único archivo de lectura. Plan pasa por `AgentRunner` con journal y ninguna herramienta ejecutada. Se cierra y reabre el store, se admite Build y `ResumeEngine` recupera ese turno desde `AdmittedNotStarted`. Build sólo anuncia `read_file`, verifica su resultado y el marcador en la respuesta, y vuelve a abrir el journal para validar el replay. El adaptador OpenAI también comprueba la persistencia del ID y de los elementos originales de Responses. Los límites son tres pasos de modelo y una llamada de lectura para Build; las peticiones pueden generar costos.

Los tests no imprimen claves, prompts, respuestas ni eventos. Los endpoints son los oficiales fijados por los adaptadores del producto. El workspace y su journal temporal se eliminan al terminar la prueba. Un fallo sólo informa la fase; para investigar datos del proveedor se necesita un entorno de depuración privado.

Esta prueba cubre la recuperación de un turno **admitido pero aún no iniciado** y la continuación desde el historial de un turno completo tras reabrir el store. No simula una caída durante una petición HTTP, un efecto de herramienta incierto, compactación, UI ni subagentes. El éxito requiere que el modelo configurado coopere con una llamada de lectura; no se reintenta una respuesta inesperada.
