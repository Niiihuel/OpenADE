# Plugins ejecutables de OpenIDE

Un plugin es un paquete local con código propio y un manifiesto `openide-plugin.json`. Su proceso ofrece herramientas al agente nativo mediante MCP stdio. El gestor mantiene una copia privada, una identidad SHA-256 y la activación de cada proyecto. Marketplace sigue administrando skills; la configuración MCP sigue admitiendo conexiones independientes.

Este formato no carga VSIX, módulos WASM de Zed ni APIs de extensión de esos editores. Tampoco declara todavía paneles, temas, hooks o skills dentro del paquete.

## Manifiesto

```json
{
  "schema_version": 1,
  "id": "project-info",
  "name": "Información del proyecto",
  "version": "1.0.0",
  "description": "Nombres y contadores del primer nivel del proyecto.",
  "publisher": "OpenIDE",
  "program": "python3",
  "entrypoint": "server.py",
  "args": ["${workspace}"],
  "credential_env_names": []
}
```

El ID empieza con una letra minúscula y contiene hasta 24 caracteres `a-z`, `0-9`, `_` o `-`. La versión es una etiqueta del autor; no hay actualizaciones automáticas ni resolución de dependencias. `publisher` identifica lo declarado por el paquete, no una firma verificada.

`program` puede ser un nombre de ejecutable disponible en PATH, con `entrypoint` obligatorio dentro del paquete, o una ruta relativa de un ejecutable incluido, por ejemplo `./bin/server`. En Unix este último necesita permiso de ejecución. No se aceptan rutas absolutas, shell commands ni entradas fuera del paquete. Con intérprete, el comando efectivo es `program <ruta instalada de entrypoint> <args...>`; los argumentos se pasan después de la entrada, no como flags del intérprete.

Los argumentos son literales. Se sustituyen únicamente `${plugin}` y `${workspace}`, como argumento completo o al comienzo de una ruta (`${plugin}/data/schema.json`). No se admite `..` ni interpolación de otras variables. El directorio de trabajo del proceso es el proyecto canónico. Un argumento literal absoluto puede referirse a otra ruta; esta API no promete aislamiento del sistema de archivos.

`credential_env_names` contiene sólo nombres. Sus valores se resuelven desde el entorno al iniciar el proceso y no se guardan en el manifiesto ni en el registro. No incluyas secretos en `args`, que son visibles durante la revisión y se persisten como parte del paquete.

## Instalación, activación y ejecución

1. Revisar una carpeta valida su manifiesto y calcula el hash de todos los archivos y rutas incluidos. No ejecuta código.
2. Instalar vuelve a comprobar esa observación, copia el contenido a almacenamiento privado y verifica la copia. Queda desactivado en todos los proyectos. Si el origen cambió desde la revisión, hay que revisarlo nuevamente.
3. Activar verifica la integridad del paquete y el catálogo MCP combinado antes de guardar el proyecto canónico. No ejecuta el proceso. Se rechazan IDs duplicados y más de 16 servidores entre conexiones manuales y plugins activos.
4. Un turno Construir obtiene los plugins activados y verifica otra vez el contenido. El runtime MCP inicializa los procesos, descubre herramientas `mcp__plugin_<id>__<tool>`, aplica los permisos de herramientas y cierra los procesos al terminar o cancelar el turno. Plan no inicia estas herramientas externas.
5. Desactivar afecta los turnos siguientes. Se puede desactivar una copia alterada o ausente para recuperarse de ese estado. El snapshot de un turno ya iniciado no cambia silenciosamente.
6. Quitar elimina el registro y la copia gestionada para todos los proyectos. El origen elegido permanece intacto. El host impide quitar paquetes durante ejecuciones locales activas; el backend no mantiene leases entre procesos independientes de OpenIDE, por lo que no se debe quitar un paquete usado por otra instancia.

La instalación vive en `plugins/installed/<id>` bajo el directorio de datos de la aplicación. `plugins/registry.json` conserva manifiesto, hash y proyectos activados; un bloqueo de archivo serializa sus modificaciones y cada escritura usa reemplazo atómico. Los paquetes no se actualizan en el lugar: quita la versión anterior y revisa una nueva instalación. Si una actualización cambia el contenido, debe volver a activarse.

El hash incluye dependencias, archivos auxiliares y el permiso ejecutable en Unix. El plugin debe escribir datos y cachés fuera de su carpeta instalada; por ejemplo, un servidor Python puede establecer `sys.dont_write_bytecode = True` antes de importar módulos propios. Si cambia cualquier archivo incluido, el siguiente inicio se bloquea. El hash detecta cambios frente a la copia registrada; no firma al autor ni convierte el proceso en un sandbox. El código del plugin se inicia con los permisos del usuario, incluso antes de que una llamada concreta a una herramienta solicite aprobación.

## Límites y validación

- Manifiesto: 64 KiB, campos desconocidos rechazados y versión de esquema 1.
- Paquete: 64 MiB en total; 16 MiB por archivo; 2.048 archivos; 512 carpetas; 32 niveles.
- Sólo archivos regulares y carpetas, con nombres UTF-8; se rechazan enlaces simbólicos y rutas de escape.
- Hasta 64 paquetes instalados y 128 proyectos activados por paquete.
- Hasta 32 argumentos, contando la entrada, y los mismos límites de argumentos y variables de credenciales que MCP.
- No hay descarga de dependencias ni scripts de instalación automáticos.

Las pruebas del backend cubren instalación inactiva, persistencia, activación por proyecto, cambios de contenido, rutas, enlaces simbólicos, permisos ejecutables, límites y conservación del origen. Las pruebas de integración del runtime ejercitan MCP real, permisos de llamada, ausencia de arranque en Plan y cierre. El [ejemplo project-info](../examples/plugins/project-info/README.md) tiene además pruebas de protocolo stdio, límites de salida y ausencia de lectura de archivos.

## Referencias de arquitectura

Zed declara sus capacidades en `extension.toml` y ofrece una API propia para módulos WebAssembly ([API oficial](https://github.com/zed-industries/zed/blob/main/crates/extension_api/README.md)). VS Code separa el manifiesto, las contribuciones y el host de extensiones ([anatomía](https://code.visualstudio.com/api/get-started/extension-anatomy), [host](https://code.visualstudio.com/api/advanced-topics/extension-host)). OpenIDE conserva esa separación entre identidad, instalación y ejecución, usando el runtime MCP nativo que ya existe en el proyecto.
