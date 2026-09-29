# Selección de skills locales del harness nativo

`openide-harness::discover_skills` lee `SKILL.md` sólo de los hijos directos de
raíces que el host haya autorizado explícitamente. No activa ninguna skill. El
catálogo rechaza nombres duplicados sin distinguir mayúsculas, documentos
inválidos, enlaces simbólicos o puntos de análisis de Windows, rutas que
escapan de la raíz y archivos superiores a 128 KiB.

El host muestra el catálogo al usuario y guarda cada elección como
`DiscoveredSkill::selection()`. La selección contiene el nombre, origen
(`project` o `user`), raíz y rutas canónicas, y SHA-256 del documento completo.
Antes de construir **cada** turno nativo, el host llama
`activate_skills(roots, selections)`. Esta función redescubre el catálogo y
exige coincidencia exacta con cada selección guardada. Un cambio de nombre,
ruta o contenido, una skill eliminada, una selección repetida o una entrada
insegura abortan la activación completa. Nunca se incluyen por defecto las
skills descubiertas. La salida conserva el orden estable del catálogo, limita
la activación a 16 skills y 256 KiB de texto total, y devuelve un digest que
vincula el texto resultante con sus procedencias.

`agents-window` expone una selección propia en Settings → Skills, separada del
Marketplace. Las únicas raíces del catálogo local se derivan del proyecto
actual elegido por el usuario y de la carpeta de datos de OpenIDE:

- Proyecto: `<workspace>/.agents/skills`.
- Usuario: `<data_root>/local-skills`.

No se crean ni leen otras raíces por búsqueda ambiental. La ausencia de una
raíz produce un catálogo vacío para ese alcance. Un enlace simbólico, junction
o punto de análisis en los componentes de estas raíces falla el descubrimiento
completo. Las rutas de la selección se guardan canónicas. Nombres duplicados
entre ambos alcances también bloquean el catálogo hasta que se resuelvan.

El archivo `<data_root>/native-local-skills.json` guarda selecciones exactas
por proyecto canónico, con versión 1. La lectura limita el archivo a 512 KiB,
128 proyectos y 16 skills por proyecto. Un archivo inválido bloquea los turnos
nativos. Settings ofrece un restablecimiento explícito y confirmado que borra
las selecciones de **todos** los proyectos; no se reemplaza silenciosamente.
El guardado normal exige que el catálogo completo y todas las selecciones
queden válidos y usa reemplazo atómico.

La UI muestra los documentos disponibles y las selecciones guardadas, incluso
si una selección quedó obsoleta. Un documento cambiado requiere que el usuario
elija «Aceptar cambios» para guardar su nuevo hash. Una selección cuyo
documento desapareció se puede quitar. «Actualizar lista» repite el
descubrimiento, pero no activa nada por sí mismo.

Antes de iniciar o reanudar cada turno nativo, el host recarga la selección
desde disco, ejecuta `activate_skills` y agrega sus instrucciones y
`ActivatedSkills.sha256_hex` al contexto del sistema. Así el digest durable del
turno cubre texto y procedencia exacta. Si una skill cambió después de
interrumpirse el turno, la reanudación se bloquea hasta una nueva selección
explícita; el catálogo antiguo no se reutiliza. Las automatizaciones nativas
usan la misma activación del proyecto al despachar cada ejecución.

Los archivos referenciados por una skill no se cargan automáticamente. Una
skill seleccionada puede pedir al agente que lea otros recursos mediante las
herramientas y permisos normales. Las comprobaciones de rutas reducen cambios
y enlaces inesperados durante el descubrimiento; no ofrecen aislamiento frente
a un proceso hostil que sustituya directorios de forma concurrente.
