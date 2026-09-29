# Arquitectura

```text
agents-window (aplicación nativa)
  ├─ openide-ui       árbol de elementos, layout, eventos y dibujo
  ├─ openide-core     sesiones, eventos y persistencia
  ├─ openide-harness  bucle de agente y herramientas nativas
  └─ openide-cli      ejecución de agentes CLI externos

Git y GitHub se consultan en workers separados para mantener el renderizado
fluido incluso si un checkout o la red responden con demora.
```

La interfaz se mantiene separada del runtime. Ambos backends emiten el mismo
flujo de eventos; la ventana lo presenta y lo conserva en una sesión. Un perfil
CLI se define por comando, argumentos y protocolo de salida. El harness nativo
usa proveedores y herramientas registrables.

La UI mantiene un registro de pestañas del dock derecho por proyecto y tarea.
El composer y el dock derecho comparten una superficie y se separan por un
divisor de 1 px; el dock de sesiones conserva su propia superficie. El host
anima los anchos de los docks durante 200 ms y vuelve a calcular la geometría
interactiva en cada frame. Las animaciones de selección y de popovers viven en
`openide-ui`, asociadas a IDs estables. `OPENIDE_REDUCED_MOTION=1` desactiva las
transiciones decorativas.
`FileExplorer` carga el árbol de forma perezosa y el lector de preview comprueba
que cada archivo permanezca dentro del proyecto. `SessionStore` guarda metadatos,
eventos, borradores y decisiones de permisos en archivos separados. Las
decisiones se sincronizan antes de liberar una herramienta bloqueada por el
broker de permisos del harness.

## Reglas de procedencia

1. `references/warp`, `references/ZCode` y `references/codex` son clones de lectura y quedan
   excluidos por `.gitignore`.
2. No se copia código de las partes AGPL de Warp.
3. El framework de UI implementa conceptos documentados por Warp con código
   original. Si en el futuro se adapta código MIT de Warp, debe conservarse su
   aviso de copyright y atribución.
4. La referencia funcional a ZCode no equivale a paridad de implementación.
   Las funciones pendientes se registran explícitamente al verificarlas.
5. El harness nativo usa Codex como referencia. La decisión vigente y los
   criterios verificables están en [native-harness-roadmap.md](native-harness-roadmap.md).
