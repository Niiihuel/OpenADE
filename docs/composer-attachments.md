# Adjuntos del composer

La referencia visual es [Attachment de Rust/UI](https://rust-ui.com/docs/components/attachment).
La implementación es propia de `openide-ui`, basada en `Element` y `UiTree`.

## Presentación

`Attachment` admite orientación horizontal para archivos y vertical para
imágenes. Los archivos muestran nombre, tipo y tamaño cuando se puede leer.
En el composer las imágenes usan `.preview_only()`: miniaturas cuadradas de
96 px, con el nombre y los detalles en el tooltip. El botón de quitar es una
acción independiente de 24 × 24 px, circular y anclada a la esquina superior
derecha. `.open_action(...)` permite abrir la imagen sin activar la eliminación.
Las imágenes se ajustan sin recortar el contenido y conservan bordes redondeados.
La falta de metadatos se indica como “Dato no disponible”; una imagen que no
puede decodificarse indica “Vista previa no disponible”.

Los adjuntos aparecen arriba. Debajo, el editor comparte una fila con el botón
de agregar y los controles de envío; al crecer el texto, los botones mantienen
su alineación inferior.
El alto se obtiene de las líneas ajustadas al ancho y de las métricas de la
fuente del renderer, sin estimar caracteres por línea. El máximo muestra líneas
completas y el scroll interno mantiene visible el cursor al superarlo.
El composer agrupa imágenes y archivos, acomoda las tarjetas al ancho disponible
y limita el área de adjuntos a 160 px con scroll. El carril de la barra queda
fuera de las tarjetas. Los índices originales se conservan para quitar el
adjunto correcto aunque los grupos cambien de orden.

Los botones superpuestos usan `Element::overlay(...).clip_children()`: se
pintan dentro de la tarjeta y respetan el recorte de su scroll. Sus tooltips
siguen siendo flotantes. El tamaño se fija tanto en el botón como en el
contenedor del tooltip, para evitar que el layout lo estire a toda la tarjeta.

## Permisos y modo

El composer nativo muestra un único selector de modo y permisos junto al
botón de agregar. Plan se activa de forma independiente; un divisor separa
esa opción de los presets de permisos. Desactivar Plan recupera el modo
Agente con los permisos elegidos, sin sustituirlos.
El selector permanece en la misma fila que el botón `+` y el editor. El
presupuesto de ancho reserva su tamaño antes de acomodar modelo y envío;
en ventanas estrechas se reduce a su icono, con la etiqueta en el tooltip.
`PermissionSelector` y `PermissionOption` pertenecen a `openide-ui`; el host
conserva su selección y utiliza la configuración real de herramientas.
El menú utiliza el perfil COMPACT: 256 px de ancho preferido y filas de
32 px con etiqueta, icono y selección. Las explicaciones se muestran en
tooltips laterales. Las medidas de permisos y catálogos están centralizadas
en [el estándar de selectores](ui-selector-standard.md).

- **Sólo lectura** desactiva edición y comandos locales, conservando los
  permisos de lectura configurados.
- **Pedir aprobación** habilita edición y comandos con la política de
  aprobación del motor. El aislamiento y las reglas de comandos siguen
  aplicándose; un comando bloqueado no se vuelve ejecutable por este selector.
- **Personalizado** abre Configuración → Agente para elegir herramientas,
  aislamiento y reglas.

MCP y navegador conservan su aprobación independiente. La configuración se
guarda mediante el mismo flujo de ajustes y se aplica a los próximos turnos;
el selector queda bloqueado durante una respuesta. Estos presets no representan
una clasificación automática de acciones seguras.

Plan permite las lecturas locales certificadas y disponibles, excluye cambios,
comandos y herramientas externas, y conserva el plan propuesto en el journal.
Agente utiliza las herramientas configuradas. El modo elegido está visible;
pedir un plan en lenguaje natural no cambia la política del motor.

La estructura se tomó del código de
[V4ComposerModeControls de ZCode](https://github.com/zai-org/ZCode/blob/29628c9acdb81b703bbd4080c207a0e7ce5e276e/packages/ui/src/v4/composer/V4ComposerModeControls.tsx)
y su ajuste de barra en `ChatPromptEditor.tsx` y `useComposerToolbarFit.ts`.
La implementación utiliza los componentes propios de `openide-ui`.

## Comandos, skills y referencias

Escribir `/` al principio de una línea abre sugerencias agrupadas de comandos
y skills del catálogo real. `/plan` y `/agent` cambian el modo visible;
al enviar `/plan <pedido>` o `/agent <pedido>`, el prefijo se aplica al modo y
el modelo recibe el pedido restante. El comando solo cambia el borrador sin
crear un turno vacío. No se admite un cambio de modo en la cola de un turno
activo: el borrador se conserva para enviarlo cuando termine.
`/permissions` abre permisos, `/skills` abre el catálogo,
y `/files` inicia una referencia con `@`.

Las skills se presentan con su nombre, descripción y origen. Elegir una abre
la revisión existente de sus instrucciones y conserva la ruta/hash observado.
Su activación sigue requiriendo aceptar esa revisión y se valida al admitir el
turno; la sugerencia no ejecuta instrucciones.

`@` abre archivos reales de la carpeta activa. Sin consulta presenta los
archivos de la raíz; al escribir busca también dentro de subcarpetas mediante
el buscador acotado existente. La búsqueda corre fuera de la UI, con debounce
de 100 ms, una solicitud pendiente y un resultado pendiente. Los resultados
de otra carpeta, consulta o generación se descartan. Hay hasta 32 sugerencias,
con un listado virtual de hasta siete filas visibles, scroll mediante rueda
y barra, y desplazamiento automático al navegar con el teclado. La altura
se reduce en ventanas bajas; el título y la ayuda permanecen visibles.

↑/↓ cambia la selección, Enter o Tab acepta, y Escape cierra hasta cambiar
el texto o el cursor. Shift+Enter y Ctrl+Enter conservan salto y cola.
El popup no captura el foco de escritura. Aceptar una referencia sustituye
sólo el token bajo el cursor, añade un adjunto real y mantiene el resto del
borrador. Las rutas se validan dentro del proyecto y se escriben entre comillas
para admitir espacios. Se conserva el límite de adjuntos y el pipeline de
validación al enviar.

Se consultaron las implementaciones locales de Zed en `completion_provider.rs`,
`message_editor.rs`, `profile_selector.rs` y `agent_settings.rs`, y las de ZCode
en `slashCommandHelpers.ts` y `mentions/providers/fileMentionProvider.ts`.
La implementación de controles, sugerencias y worker es propia en Rust.

## Vista completa y galería

Al hacer clic en una miniatura se abre el componente propio `ImageViewer`.
Se ajusta a la ventana, muestra el archivo original con su proporción completa
y ofrece cierre con X o Escape. Cuando hay varias imágenes se muestran anterior,
siguiente y el contador; las flechas del teclado también navegan. En los extremos
se deshabilita la dirección que no tiene otra imagen. El foco queda dentro del
visor y vuelve al editor al cerrar.

El host conserva una lista de las imágenes que estaban adjuntas al abrirlo.
El worker lee la imagen seleccionada y etiqueta cada solicitud con una
generación: un resultado anterior no puede sustituir la imagen elegida después.
Se muestran estados de carga y error sin bloquear la ventana.

`ImageViewer` sólo recibe `RasterImage`, nombre, posición y acciones del host;
puede reutilizarse en otras vistas. Solicita `.full_resolution()` al renderer,
que distingue esas texturas de las miniaturas. La vista completa mantiene los
límites de 8 MiB de archivo, 4096 px por lado y 12 megapíxeles, con 64 MiB de
presupuesto de decodificación. La caché conserva como máximo dos imágenes
completas, aparte de las 16 texturas de miniaturas. Ver una imagen no modifica
su archivo ni el borrador.

## Pegado y almacenamiento

Con el editor enfocado, Enter envía y Shift+Enter inserta un salto de línea.
Ctrl+Enter pone el mensaje en la cola del turno nativo que está respondiendo;
en reposo conserva el borrador y no lo envía. Se utiliza la cola persistente del
motor, incluidos los adjuntos, y el envío espera a que termine su preparación.
Los menús y los diálogos mantienen sus propios atajos.

Enviar y Detener comparten el mismo lugar del composer. Durante una respuesta
sólo aparece Detener, con el icono blanco centrado; la acción de poner en cola
se conserva en Ctrl+Enter sin añadir otro botón de envío junto a él.

- Con el editor del composer enfocado, un pegado de rutas `file://` adjunta los
  archivos. El texto común mantiene el comportamiento de pegado de texto.
- Para imágenes del portapapeles, el host usa la liberación de V con Ctrl/Cmd
  todavía pulsado: `egui-winit` consume la pulsación inicial sin emitir un
  evento de pegado cuando no hay texto. El menú **Archivos y carpetas → Pegar
  imágenes o archivos** permite el mismo pegado sin depender de ese gesto.
- Las rutas del proyecto se usan directamente. Los archivos externos se copian
  a `.openide/attachments/<uuid>/<nombre-original>`. Las imágenes del
  portapapeles se guardan como PNG en esa misma estructura.
- La carpeta de importación incorpora un `.gitignore` con `*` si no existe;
  no reemplaza uno existente. El pegado copia también cuando el gestor de
  archivos indica `cut`: nunca mueve ni borra el archivo de origen.
- Quitar una tarjeta sólo retira el adjunto del borrador. Las copias se
  conservan para las referencias de borradores y mensajes; no hay limpieza
  automática de ese almacenamiento en esta versión.

El worker hace las copias, consulta metadatos y genera miniaturas fuera del
hilo de UI. Mientras prepara un pegado, el composer muestra una miniatura
provisional con un pulso suave, sin spinner ni texto, y bloquea el envío.
Un resultado pertenece al proyecto y borrador que lo pidió,
aunque se cambie de tarea mientras termina la operación.

## Selector de carpetas del sistema

**Elegir otra carpeta…** del proyecto y **Adjuntar carpeta del proyecto**
comparten el diálogo nativo de `apps/agents-window/src/folder_dialog.rs`.
En Linux se consulta primero el [portal FileChooser](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html)
mediante `ashpd`; el escritorio determina qué selector presenta, tanto en
Wayland como en X11. La solicitud permite seleccionar una sola carpeta e
inicia en la carpeta actual o su ancestro existente más cercano.

Si el portal no está disponible o falla, se usan los selectores instalados:
KDialog primero en KDE, Zenity primero en los demás escritorios, y luego el
otro como respaldo. Cancelar termina la operación sin abrir otro diálogo ni
modificar el proyecto o los adjuntos. Los errores de disponibilidad se muestran
en el composer. El diálogo corre fuera del hilo de UI y se impide abrir dos
selectores de carpetas a la vez. Windows usa `FolderBrowserDialog`.

## Límites y conexión al motor

Se conservan los 12 adjuntos del composer. Cada archivo externo puede ocupar
hasta 64 MiB; una imagen pegada puede ocupar hasta 8 MiB en PNG. La lectura para
miniaturas se limita a 8 MiB, el decodificador tiene un presupuesto de 64 MiB
y dimensiones máximas de 16.384 px por lado. La miniatura tiene como máximo
256 × 256 px. La caché conserva hasta 128 entradas y las colas son acotadas.

El modelo de borrador sigue usando `path`, `label` e `is_dir`, con rutas
relativas al proyecto. La miniatura pertenece únicamente a la presentación;
el envío nativo vuelve a leer y validar la imagen original mediante el contrato
de [imágenes de usuario](native-user-images.md), incluidos los límites de 8
imágenes y 24 MiB por mensaje y la capacidad de entrada del modelo. Los perfiles
CLI conservan su mecanismo actual de referencias a archivos del proyecto.
