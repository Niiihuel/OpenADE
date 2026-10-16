# Conexión de los módulos de Settings

Este incremento conecta controles con capacidades que ya ejecuta el IDE. La
referencia visual y funcional es el [inventario de Cursor](research/cursor-settings/README.md);
los permisos y la persistencia siguen los contratos propios de openADE.

## Navegador

Settings permite habilitar la automatización, elegir el destino de enlaces y
vistas previas y administrar permisos por origen. Desactivar la automatización
retira las herramientas de los turnos nuevos y rechaza llamadas de clientes ya
abiertos; el navegador manual sigue disponible. El valor inicial conserva el
comportamiento existente.

Un bloqueo por sitio se comprueba al ejecutar y antes de devolver contenido,
también con pestañas, historial y redirecciones. Una configuración de permisos
inválida detiene la automatización y se muestra en Settings; su recuperación
requiere una acción explícita con copia del archivo original. No se interpreta
un archivo inválido como una lista vacía de bloqueos.

## Subagentes

Settings configura la delegación y los límites que ya aplica el coordinador.
Los cambios se guardan para nuevos turnos; los workers activos conservan su
perfil. Las recuperaciones siguen verificando la identidad guardada y requieren
la revisión existente si cambió la autoridad. Desactivar la delegación conserva
el seguimiento y la cancelación de hijos existentes.

El catálogo muestra los roles incorporados y las personas de plugins aprobados.
No ofrece edición de roles ni modelos independientes que el motor no admite.
`explorer` y `reviewer` conservan su modo de lectura también al continuarlos.

## MCP

La página edita por separado la configuración del usuario y la de este proyecto.
El perfil del proyecto se guarda en el directorio privado del IDE, ligado a la
ruta canónica del workspace; no se ejecutan automáticamente archivos traídos
por un repositorio. La configuración global existente conserva su ruta. En la
configuración efectiva, el proyecto reemplaza al usuario por ID, incluso si su
entrada está deshabilitada. La inspección y OAuth pertenecen al ámbito elegido.

El fallo de un servidor no impide usar los demás. Los diagnósticos y previews
ocultan credenciales, incluidos encabezados obtenidos del entorno. La importación
local admite las configuraciones de Cursor además de las fuentes existentes y
presenta errores de lectura o formato sin habilitar servidores por sí sola.

## Resto de las rutas y aceptación

Editor, Terminal, Skills, Plugins, Hooks, Comandos, Memoria, Automatizaciones,
Datos y Proveedores conservan sus servicios y archivos existentes. La revisión
comprueba controles, acciones, guardado y uso real antes de añadir otro sistema.
Una vista previa de documento no debe impedir cambiar preferencias del editor
desde Settings ni permitir que controles obsoletos editen el documento oculto.

La aceptación usa fixtures temporales, render y eventos nativos y servicios
HTTP/CDP locales. Comprueba reapertura, ámbitos y permisos en el runtime, sin
usar credenciales ni modificar servidores o cuentas del usuario. La suite del
IDE y el build de escritorio completan la validación del incremento. Las pruebas
Linux no certifican por sí solas la aceptación nativa en Windows o macOS.

La revisión conserva estas conexiones existentes:

- General, Terminal y Atajos usan `SettingsPreferences`, el lanzamiento de PTY
  y el keymap del editor. Editor conserva `editor-preferences.json` y su writer.
- Agente, Comandos y Hooks usan las políticas de herramientas, reglas de argv y
  registros de hooks que consume el harness. No se agregan permisos paralelos.
- Proveedores y Uso conservan conexiones, catálogo, presupuesto y observaciones
  de cada proveedor; GitHub conserva su worker y la selección de cuenta.
- Skills y Plugins conservan selección por proyecto, revisión, activación y
  carga de instrucciones/herramientas. Las personas se muestran desde esas mismas
  raíces aprobadas, sin otro catálogo de perfiles.
- Memoria conserva contexto, instrucciones guardadas, checkpoints y objetivos
  nativos. No ofrece una memoria aprendida que el motor todavía no implementa.
- Automatizaciones conduce a la superficie que administra sus tareas guardadas;
  su scheduler sigue funcionando mientras el IDE está abierto. Datos conserva
  organización, orden y acceso a la carpeta del perfil.
- CLIs externas conserva los grants revocables de cada terminal y sus permisos
  independientes. La delegación nativa a través de una CLI externa permanece
  deshabilitada en la interfaz porque ese puente aún no la implementa.

## Revisión de presentación

Marketplace concentra el catálogo y sus instalaciones; MCP y Plugins conservan
su administración en Ajustes. Orígenes es un modal del catálogo que conserva
consulta, filtros y selección. Las acciones MCP de una pantalla anterior se
rechazan cuando su página ya no está visible.

Navegador y Plugins reciben el ancho real de la columna de Ajustes: los selectores,
campos y barras de acciones se adaptan también a 320 px. La etiqueta corta de
permisos del navegador conserva las explicaciones y la política de ejecución.
Los prefijos de comandos usan su altura de contenido hasta 80 px, con scroll para
patrones largos. Un error de carga de hooks ofrece recargar el archivo corregido
sin cambiar ni reemplazar sus reglas.

Skills conserva la selección local verificada y su revisión antes de activar
instrucciones; queda como mejora de navegación filtrar ese catálogo local dentro
de Ajustes. Subagentes conserva límites, perfiles y controles de recuperación
conectados al coordinador existente.
