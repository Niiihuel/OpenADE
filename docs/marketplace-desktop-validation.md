# Validación nativa de Marketplace

## Recorrido reproducible

`scripts/marketplace-desktop-e2e.py` requiere un binario compilado con `editor-e2e` y el compositor privado de `scripts/headless-desktop-fixture.py`. Rechaza ejecutarse sobre el escritorio normal. Todas las entradas pasan por teclado y puntero Wayland de esa instancia; el observer sólo publica estado y geometría, no acepta órdenes.

La fixture crea un proyecto y un origen local temporal con `native-fixture-skill/SKILL.md`, sin código ejecutable ni credenciales. La aplicación empieza con su store vacío y carga las recomendaciones reales. Un segundo servidor HTTP propio responde al protocolo de búsqueda, metadatos Git y descarga de paquetes; el override `OPENIDE_E2E_MARKETPLACE_ORIGIN` sólo existe en builds `editor-e2e`, exige una URL de loopback y no modifica las rutas de producción.

El recorrido ampliado comprueba:

1. Recomendaciones iniciales sin orígenes propios ni instalación automática, geometría a 1000 × 820 y 520 × 640, categorías y paginación de 24 entradas como máximo.
2. Detalles y procedencia, cierre con Escape, filtros vacíos y modales de origen con foco correcto.
3. Escribir en Explorar no inicia red; Buscar produce un estado en curso y un resultado del servidor loopback. Instalar copia un paquete completo, incluidas sus referencias, con contenido exacto en disco.
4. Buscar actualizaciones conserva los archivos anteriores. Actualizar explícitamente aplica la nueva revisión y conserva el ámbito y el estado desactivado.
5. La búsqueda recupera una copia guardada cuando el origen falla; una consulta sin caché muestra error y Reintentar. Un resultado vacío después de recuperar conexión no conserva botones de instalación anteriores.
6. Un origen local inválido produce un error recuperable; agregar la carpeta válida permite instalar en Proyecto. Desactivar, cerrar y volver a abrir conserva la instalación y su estado.
7. Cancelar desinstalación conserva la copia; confirmar desinstalación y quitar origen conserva la carpeta original y las recomendaciones.
8. Complementos en modo IDE reutiliza páginas acotadas, búsqueda lateral, detalle central y navegación Volver. A 520 px el lateral funciona como drawer. El observer comprueba que no haya IDs duplicados, salvo la pareja intencional de viewport y scrollbar.

El entorno hijo excluye credenciales heredadas y bloquea sondeos externos de cuentas; un proxy local rechaza tráfico HTTP externo. No se descarga ninguna recomendación publicada ni se abre un repositorio externo. El recorrido HTTP usa sólo respuestas y archivos generados por la propia fixture. El observer no acepta órdenes: todos los controles se operan por teclado y puntero Wayland privados.

## Ejecución

```bash
rtk nix-shell --expr 'let pkgs = import <nixpkgs> {}; in pkgs.mkShell { packages = [ pkgs.stdenv.cc pkgs.pkg-config ]; LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.wayland pkgs.libxkbcommon pkgs.vulkan-loader pkgs.libGL ]; }' --run 'rtk python3 scripts/headless-desktop-fixture.py --kwin /nix/store/4ghnrbsfcxsvf65d4rhb2h220wj8yc7s-kwin-6.7.2/bin/kwin_wayland --output /tmp/openide-marketplace-private -- python3 scripts/marketplace-desktop-e2e.py --binary /tmp/openide-marketplace-e2e-bin --wtype /nix/store/h778lc6jph6mpazfnhzb0la0pgvrinw7-wtype-0.4/bin/wtype --output /tmp/openide-marketplace-native'
```

Las rutas Nix identifican los paquetes utilizados en este entorno; en otro equipo deben apuntar a sus ejecutables disponibles. El runner crea un KWin virtual privado y Hyprland anidado, y elimina sólo sus propios procesos y sockets al terminar. El script cierra únicamente su aplicación de fixture y conserva los artefactos temporales para inspección.

## Evidencia actual

Ejecución ampliada del 7 de octubre de 2026: **10/10 recorridos aprobados**, sin rutas alternativas ni reintentos automáticos de texto. Se utilizó `/tmp/openide-marketplace-e2e-bin` sobre eframe/wgpu Wayland nativo, con KWin virtual privado → Hyprland anidado. El runner terminó con código 0.

- Reporte: `/tmp/openide-marketplace-native/report.json`.
- **34 capturas PNG** en `/tmp/openide-marketplace-native/`, con recomendaciones, categorías, página siguiente, búsqueda, estados en curso/vacío/error/caché, procedencia, actualización, instalación local y Complementos IDE a ambos anchos.
- Dos procesos de la fixture `/tmp/openide-marketplace-e2e-ot6im5ik`, PID 2001965 y 2008685. Ambos se comprobaron terminados; los dos logs quedaron en 0 bytes.
- Sin credenciales creadas ni peticiones a hosts externos registradas por el proxy. Las 15 peticiones del recorrido remoto fueron al servidor loopback propio.
- Acciones **Instalar** completas de 88 × 28 px, tarjetas visibles de aproximadamente 114–128 px, dos columnas a 1000 px y una a 520 px. Los controles de tarjetas parcialmente visibles se recortan en el borde del viewport; no se confunden con botones completos de menor altura.
- La instalación remota preservó exactamente `SKILL.md` y `references/info.txt`. Comprobar actualizaciones mantuvo la revisión anterior; pulsar Actualizar aplicó la siguiente y conservó Proyecto + Inactiva. Las dos revisiones y el botón fijo del modal se alcanzaron mediante scroll real.
- Buscar por botón y por Enter funcionó. El refresco forzado sin conexión marcó la copia como guardada sin conexión. Una consulta sin copia mostró Reintentar; la recuperación devolvió una lista vacía sin acciones anteriores.
- La paginación lateral de IDE cambió de página con controles físicos propios y acciones compartidas. La búsqueda y Volver conservaron la consulta; el drawer compacto se cerró al abrir un detalle, sin IDs duplicados.

Durante la integración se detectaron y corrigieron dos fallos de UI: el campo local heredaba flex vertical de la fila de Explore y empujaba el estado vacío; el alias de actualización quedaba en un Tooltip externo y despachaba la operación equivocada. Las regresiones comprueban altura estable y clics físicos de refresco/paginación. El recorrido final volvió a cubrir ambos sin cambiar la intención de los controles.

La revisión de capturas detectó además que la fuente no contiene una flecha Unicode entre SHA. La copia final evita ese glifo: presenta **Revisión instalada** y **Revisión disponible** por separado, con fallback explícito cuando no hay revisión guardada. Este ajuste aprobó su test focal y el export; `target/marketplace-visual-qa/update-details-520.png` se inspeccionó con ambas revisiones separadas y sin glifo faltante. El recorrido funcional conserva la captura previa de ese texto.

La integración central aprobó **69 tests de Marketplace y 2 exports ignorados por defecto**, incluido el último ajuste de texto. El test de export se ejecutó por separado y pasó. El export `export_marketplace_visual_review` ahora incluye 30 imágenes de 10 variantes a 280/520/1000 px, incluido detalle de actualización. La comprobación de alcance de 1.500 entradas valida que sólo se construyan 24 tarjetas por página.

Como validación separada de la fixture nativa, la integración central instaló **36/36 recomendaciones publicadas**, verificando 318 archivos y aproximadamente 1.84 MB de contenido. Esa prueba usa sus propias copias temporales; el recorrido nativo no descarga contenido externo.

La versión anterior de tres recomendaciones también había aprobado su recorrido local. Las regresiones conservadas cubren la cabecera estrecha, Ctrl+A en un input vacío y la sincronización entre búsquedas lateral y central.

`--ide-only` permite repetir sólo el recorrido de Complementos. No hay una opción para saltar errores de entrada. Esta validación no afirma un objetivo de FPS, compatibilidad universal de skills ni ejecución automática de instrucciones.
