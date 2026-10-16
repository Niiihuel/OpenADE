# Confetti de Cursor: preferencia, herramienta y efecto

Referencia: el mismo bundle Glass 3.17.21 y SHA-256 consignados en [README.md](README.md). Los offsets de este documento son **bytes UTF-8**, no posiciones de caracteres.

## Observaciones del paquete distribuido

- `cursor/agentConfettiCannonEnabled` es una preferencia booleana, inicialmente `false` (offset `19318268`). El switch «Give the Agent a Confetti Cannon» se oculta detrás de `glass_agent_confetti_cannon`. Con movimiento reducido efectivo, aparece desactivado y no permite cambios.
- La preferencia habilita una herramienta, `show_confetti`, del proveedor MCP `cursor-app-control`; el servicio exige flag, preferencia y ausencia de movimiento reducido también al ejecutarla (`42353668`). No se trata de una celebración automática al finalizar cualquier tarea.
- El esquema acepta únicamente un objeto con `message` y `from`, ambos opcionales y de tipo string; máximos de 200 y 100 unidades UTF-16 respectivamente (`42343093`, validación Zod/JavaScript). No admite campos adicionales. El handler elimina enlaces Markdown de los esquemas HTTP(S), command y file, conservando su texto, y recorta whitespace. Si falta `from`, puede usar el nombre del agente invocante.
- La ejecución valida el estado actual y emite `showConfettiRequested`; el resultado informa que se mostró confetti, con mensaje y atribución si corresponden (`42354883`). La disponibilidad del proveedor cambia al cambiar la preferencia, el flag o movimiento reducido.
- La guía del agente recomienda celebrar hitos verificados de trabajo sustancial, a lo sumo una vez por hito, y omitir silenciosamente la herramienta si no está disponible (`23179068`). Es una instrucción para el modelo: no se observó en estos handlers un contador duro de hitos.
- El frontend monta un canvas decorativo y un worker de efectos. Conserva como máximo cinco peticiones mientras inicializa el worker, permite un reintento de inicialización y muestra un aviso opcional de 4.500 ms (`44890072`, `44890193`). No se ejercitó ese runtime.

## Evidencia REA focalizada

Se reutilizó la investigación previa y se agregaron dos rangos contiguos exactos, sin ejecutar código de Cursor:

- Switch y configuración: `[31386453, 31387408)`, SHA-256 `1a81b202da06564efce834c9462ca05f026007171a5728b1552d6d1586b6d336`.
- Normalización y handler: `[42346322, 42346834)`, SHA-256 `a911c5413a4edb672671013bfcf85e4398b5133b8061b0749b1e165995dfa326`.

REA retuvo el análisis como `ev_965f141f2e9020c5f20a90b6297931176beca5c55ae3bb7dd1dd0c085c0fd4a2`; su vista resumen es `ev_3d10944aba7c9b36c611830be7656a1179fb88de4f6702e28c14bcd9a71c1910`. Los dos archivos, 1.467 bytes, se parsearon sin fallos. Quedan 93 fronteras semánticas desconocidas porque servicios, esquemas y referencias minificadas externos no forman parte de esos rangos. Los offsets adicionales anteriores proceden de inspección estática acotada del bundle original, complementaria al análisis REA.

La visibilidad efectiva del flag en una cuenta, la frecuencia real de uso, la física visual del worker y el comportamiento de atribución en todas las clases de agentes permanecen sin verificar. Los snippets propietarios siguen fuera del repositorio de openADE.

## Alcance de openADE

openADE no incorpora esta preferencia, el efecto visual ni la herramienta del agente. Este documento conserva solamente la evidencia de la función distribuida por Cursor.
