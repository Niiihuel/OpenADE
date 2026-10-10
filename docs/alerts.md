# Alertas nativas

`openide_ui::Alert` concentra los avisos en línea de OpenIDE. Se compone de
elementos nativos y acepta la paleta del host. Ajustes, conversación,
automatizaciones y marketplace comparten la misma presentación.

```rust
use openide_ui::{Alert, AlertTone, Button, Component};

let aviso = Alert::new(
    "No se pudo guardar",
    "Comprueba la ruta del archivo y vuelve a intentar.",
    palette,
)
.tone(AlertTone::Error)
.action(Button::new("retry_save", "Reintentar", palette).render())
.compact(ancho < 520.0)
.render();
```

## Tratamiento visual

La composición parte de [Rust/UI Alert](https://rust-ui.com/docs/components/alert):
esquinas redondeadas, título separado y descripción. Los estados semánticos
siguen [Carbon Notification](https://carbondesignsystem.com/components/notification/style/):
superficie neutral en oscuro, acento lateral de 3 px e icono de estado. Los
iconos Solar Linear mantienen los 16 px lógicos de toda la app.

`Neutral` representa un aviso sin resultado conocido; `Info` usa azul y un
círculo informativo, `Success` verde y un círculo con check, `Warning` ámbar y
un triángulo, y `Error` rojo y un círculo con X. El título expresa el significado
también mediante texto. El color no es la única señal. Un título vacío toma la
etiqueta del estado; los llamadores deberían preferir títulos específicos y
explicar cómo continuar, según las [convenciones de USWDS](https://designsystem.digital.gov/components/alert/).

`AlertColors::for_tone` conserva la superficie elevada del tema. En oscuro usa
acentos azul `#78a9ff`, verde `#6fdc8c`, ámbar `#f1c21b` y rojo `#ff8389`; en claro
usa tonos más oscuros. La tinta del título y del mensaje se corrige si la paleta
no proporciona contraste suficiente. Las pruebas comprueban texto de al menos
4.5:1 y acentos de al menos 3:1 en las paletas clara y oscura de referencia,
siguiendo [WCAG 2.2](https://www.w3.org/TR/WCAG22/).
`.colors(AlertColors { ... })` permite una paleta explícita; en ese caso el
llamador debe verificar su contraste.

Los avisos cuyo estado sólo distingue error/no-error permanecen informativos;
un mensaje en curso no se presenta como éxito por no ser un error.

## Comportamiento

El cuerpo es pasivo. Sólo las acciones suministradas participan en el foco y
emiten IDs; la capa de UI no ejecuta operaciones. `.dismiss(id)` añade el botón
Ghost de 28 px con ayuda “Cerrar aviso”; el host decide si el aviso puede
cerrarse y lo retira al recibir ese ID. No hay cierre automático ni captura de
foco. Los avisos persistentes y la colocación de acciones en pantallas estrechas
siguen [Carbon Usage](https://carbondesignsystem.com/components/notification/usage/).

La altura depende del texto. `.compact(true)` lleva la acción a una fila inferior;
en ancho amplio se alinea junto al mensaje. En modales, la validación vive en
el cuerpo desplazable y las acciones del formulario permanecen en su pie.
No se insertan alertas variables en filas virtualizadas de altura fija.

`Alert` muestra contexto sin interrumpir la tarea; `AlertDialog` conserva el
flujo modal de confirmación de operaciones. Esta API nativa no implementa
anuncios de región viva para lectores de pantalla. Las pruebas de contraste
y teclado no certifican accesibilidad completa del host.

## Verificación visual

El test ignorado `export_alert_visual_review` exporta las cinco variantes en
claro y oscuro, con anchos de 360 y 760 px, a `target/alert-visual-qa`. Usa los
comandos reales del árbol nativo y los SVG incluidos en la app. La exportación
sirve para inspeccionar geometría y composición; sus fuentes y rasterización
pueden diferir del renderer egui.
