# Información del proyecto

Plugin de herramientas para el agente nativo de OpenIDE. Requiere Python 3 accesible como `python3`; usa únicamente su biblioteca estándar y no necesita credenciales ni red.

En **Configuración → Plugins**, selecciona esta carpeta, revisa el manifiesto y confirma la instalación. La copia queda desactivada. Actívala para el proyecto que quieras consultar y comienza un turno **Construir**. El agente tendrá disponible `mcp__plugin_project-info__project_info`; sus llamadas siguen el flujo de permisos de MCP. Los turnos Plan no arrancan el proceso.

La herramienta enumera hasta 10.000 entradas del primer nivel y devuelve como máximo 100 nombres ordenados. No lee archivos ni recorre subcarpetas; tampoco sigue enlaces simbólicos. La salida distingue contadores completos de un listado truncado. Los nombres de archivos ocultos sí pueden aparecer.

El paquete se instala mediante una copia y no se ejecuta durante la revisión o instalación. No escribe cachés ni datos en su propia carpeta. Quitar el plugin borra esa copia, no esta carpeta de ejemplo.

Verifica el servidor sin instalar nada:

```sh
python3 examples/plugins/project-info/test_server.py
```

Consulta [el formato y sus límites](../../../docs/plugins.md) para crear otro paquete.
