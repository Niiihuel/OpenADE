# Codex como referencia para el harness Rust de OpenIDE

OpenIDE desarrolla su propio ciclo de agente en `openide-harness`, con eventos
durables en `openide-core` y presentación en `openide-ui`. El perfil Codex CLI
puede ejecutarse como backend externo; no proporciona el estado ni la
autenticación de las sesiones nativas. La decisión y los criterios de cierre
están en [native-harness-roadmap.md](native-harness-roadmap.md).

La comparación original se hizo con el código de Codex en el commit
[`1cc7e236`](https://github.com/openai/codex/tree/1cc7e2361237ce7244430ee1d581c77f95c57ac8).
El clon local de trabajo para el roadmap está fijado en `33a0f766` bajo
`references/codex`. Las rutas son referencias de diseño, no módulos que
OpenIDE enlace o distribuya.

| Área | Referencia de lectura | Contrato que OpenIDE debe verificar |
| --- | --- | --- |
| Turno y streaming | `codex-rs/core/src/session/turn.rs` | Solicitud, deltas, herramientas, cancelación y cierre con eventos ordenados. |
| Catálogo y ejecución de herramientas | `codex-rs/core/src/tools/{registry,router,orchestrator}.rs` | Catálogo congelado, validación de argumentos, permisos y resultado durable antes de efectos posteriores. |
| Contexto y recuperación | `codex-rs/core/src/compact.rs`, `rollout/src/recorder.rs` | Presupuesto, compactación, checkpoint y replay sin perder llamadas ni resultados. |
| Proveedores | `model-provider/src/provider.rs`, `models-manager/src/` | Separar transporte, selección y capacidades comprobadas por proveedor/modelo. |
| Protocolo para UI | `app-server-protocol/src/protocol/v2/` | Proyectar estados y aprobaciones sin duplicar la autoridad del log. |

El catálogo de OpenIDE obtiene listas de las APIs oficiales de los proveedores
admitidos y mantiene caché local. Un modelo listado no hereda capacidades de
Codex ni queda habilitado automáticamente para el harness: el adaptador Rust
debe verificar el protocolo y las opciones exactas que anuncia. Véase
[providers-models-architecture.md](providers-models-architecture.md).

El código [openai/codex](https://github.com/openai/codex) está publicado bajo
[Apache-2.0](https://github.com/openai/codex/blob/1cc7e2361237ce7244430ee1d581c77f95c57ac8/LICENSE)
y contiene un [NOTICE](https://github.com/openai/codex/blob/1cc7e2361237ce7244430ee1d581c77f95c57ac8/NOTICE).
Este documento usa ideas de diseño; si se copia código, la distribución debe
conservar la licencia y los avisos aplicables.
