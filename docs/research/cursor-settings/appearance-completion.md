# Completar Apariencia

Alcance confirmado: temas personalizados y ajuste real de rasterización de fuentes. Se extiende la implementación nativa existente; los demás módulos conservan el relevamiento de la primera etapa.

## Temas personalizados

- Crear, editar, duplicar, aplicar, exportar y eliminar temas locales, hasta 30.
- Importar YAML de temas y JSON de colores de VS Code. No ejecutar contenido, cargar `include` ni resolver recursos remotos. Validar tamaño, nombre, bucket claro/oscuro y colores antes de guardar.
- Conservar los tokens de intercambio y presentar como editables los grupos con efecto en la semántica nativa. Los aliases de colores de openADE comparten una función visual; el importador informa las diferencias de compatibilidad.
- El editor usa controles de limn: nombre, bucket, alto contraste, token, selector de color y opacidad. Guardar aplica el tema; cancelar conserva el tema guardado. Importar abre un borrador revisable.
- Los errores y la cancelación del selector de archivos conservan preferencias y tema activo. Eliminar el tema activo vuelve al tema oscuro incorporado.

## Fuentes

- El switch existente pasa a controlar `TextOptions.subpixel_binning` de egui, disponible en las plataformas del renderer. Conserva hinting, peso, familias y fallbacks.
- El perfil desactivado busca trazos más nítidos; el activado conserva la colocación subpíxel predeterminada. No equivale a desactivar antialiasing ni a seleccionar CoreText/LCD de macOS.
- Las mediciones se vuelven a adjuntar cuando egui activa la configuración en el siguiente pass.

## Implementación y verificación

Orden: modelo/importación/persistencia → editor y acciones → rasterización → integración y revisión. Los cambios de modelo y fuentes se trabajan en paralelo con archivos separados.

Pruebas Rust en los módulos correspondientes: recuperación de preferencias, importación válida/inválida, roundtrip, cambios efectivos de color, CRUD con guardado/reapertura y cancelación, activación de TextOptions y geometría/foco del editor a 360/900 px.

Comandos de integración:

```sh
rtk nix-shell shell.nix --run 'rtk cargo check -p agents-window --bin agents-window'
rtk nix-shell shell.nix --run 'rtk cargo test -p agents-window --bin agents-window -- --test-threads=4'
rtk nix-shell shell.nix --run 'rtk cargo test -p openide-ui'
rtk git diff --check
```

Las nuevas vistas reutilizan filas, selectores, feedback e inputs nativos; el layout estrecho apila los controles. Se preservan los cambios previos del workspace. No se mezclan commits de trabajo ajeno ni se publica openADE en esta etapa.
