# GitHub en el dock de cambios

La revisión combina dos fuentes separadas. `git_review.rs` lee el estado local
del checkout y agrupa diffs preparados, sin preparar y archivos sin seguimiento.
`github.rs` descubre un remoto de `github.com` y consulta los pull requests
abiertos de la rama actual. Ambos trabajos se ejecutan fuera del hilo visual,
con una generación por actualización para descartar respuestas de proyectos o
ramas anteriores.

Se aceptan remotos `https://github.com/owner/repo.git`,
`ssh://git@github.com/owner/repo.git` y `git@github.com:owner/repo.git`.
`origin` tiene prioridad. Si existen `origin` y `upstream` de GitHub distintos,
se consulta el PR de la rama de `origin` sobre el repositorio `upstream`.
Los enlaces visibles se construyen a partir del
remoto validado y del número de PR; no se abre una URL arbitraria de la
respuesta. La consulta pública funciona sin cuenta. Para repositorios privados,
el worker resuelve una credencial para la cuenta seleccionada en OpenIDE y
comprueba su identidad con `/user`. Puede provenir del entorno, GitHub CLI o
Git Credential Manager; ninguna credencial de otra cuenta se usa como respaldo.
Sin cuenta seleccionada se consulta sólo información pública. Los tokens no
entran en las vistas, sesiones ni configuración de OpenIDE. Las respuestas se
validan por cuenta, proyecto y generación, también al cambiar de cuenta.

El footer del sidebar consulta la identidad de forma asíncrona: valida tokens
del entorno o de `gh` con `/user`. `github list` sólo indica que GCM tiene
cuentas almacenadas; no demuestra que su token siga funcionando. Por eso el
footer muestra **Verificar GitHub** o **Elegir cuenta GitHub**, sin elegir la
primera cuenta si hay varias. La verificación con `get --no-ui` y `/user` ocurre
únicamente después de elegir una cuenta. Si falla, se ofrece reconectar.
«Conectar con GitHub» prepara un flujo de navegador **sólo después del clic**;
la consulta inicial no abre ventanas ni crea credenciales. El worker Rust usa
`gh auth login --hostname github.com --web` con streams no interactivos:
GitHub CLI solicita el código y consulta la autorización, mientras el diálogo
`AlertDialog` de `openide-ui` muestra el código público y controla «Copiar y
abrir navegador». El navegador predeterminado abre `github.com/login/device`.
La cuenta se valida con `/user` antes de cerrar el diálogo. Escape, Cancelar y
cierre de la aplicación detienen la espera; una generación descarta resultados
de intentos anteriores. Los códigos vencidos o rechazados ofrecen reintento.

Si GitHub CLI no está instalado, se usa el flujo de navegador de GCM. Ambos
helpers conservan sus propias credenciales; OpenIDE no copia tokens a sus
preferencias ni sesiones. GitHub CLI usa el almacén de credenciales del sistema
cuando está disponible y puede usar su propio archivo de configuración como
respaldo. Se mantiene el protocolo de Git ya configurado.
Tras validar la identidad se actualiza la revisión de GitHub activa. El botón
del usuario abre un menú compacto con identidad y estado, «Administrar GitHub»,
«Ver perfil en GitHub», «Actualizar conexión», «Conectar otra cuenta» y
«Desconectar de OpenIDE». La administración reúne estas acciones en Configuración
→ GitHub. Una reconexión fallida o cancelada conserva la cuenta verificada y
muestra el error sin reemplazar su identidad. Si GCM devuelve varias cuentas,
el diálogo permite elegir cuál verificar; la cuenta anterior sigue atendiendo
las consultas hasta que la nueva identidad se haya comprobado y guardado.

La preferencia guarda únicamente el login elegido o la desconexión de OpenIDE.
Al reiniciar se verifica esa misma cuenta; no se elige otra automáticamente.
«Desconectar de OpenIDE» cancela la verificación pendiente e invalida las
consultas de la cuenta. No borra credenciales de GitHub CLI ni GCM y se mantiene
al reiniciar. Las instalaciones anteriores conservan el descubrimiento inicial
hasta elegir una cuenta.

La apertura de enlaces usa el navegador predeterminado del sistema mediante
la feature `links` de eframe. Los botones de perfil del menú y de configuración
tienen IDs visuales distintos y comparten la acción de abrir el perfil.

El diálogo sigue el patrón de código de un solo uso, copia y continuación en
el navegador de [VS Code](https://github.com/microsoft/vscode/blob/main/extensions/github-authentication/src/flows.ts).
La autorización usa la aplicación registrada del helper instalado, no las
credenciales OAuth de VS Code. El protocolo se describe en la
[documentación de GitHub](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow)
y el almacenamiento en [GitHub CLI](https://cli.github.com/manual/gh_auth_login).

Con varias cuentas guardadas se elige explícitamente cuál verificar; el diff
local y la lectura pública siguen disponibles aunque no haya sesión de GitHub.
El componente `Avatar` de `openide-ui` muestra la foto real tanto en el footer
como en Configuración → GitHub. La URL proviene del perfil verificado; el worker
descarga una imagen de `avatars.githubusercontent.com` sin enviar credenciales,
con límites de tiempo, bytes y memoria de decodificación. La foto se recorta a
un cuadrado de 128 px y el renderer la pinta circular. Si no está disponible,
se muestra la inicial del usuario. Las comprobaciones de tokens que sólo
necesitan el login no vuelven a descargar la foto.

El dock muestra el repositorio y hasta cuatro PR abiertos para la rama actual,
con enlaces externos. Distingue carga, ausencia de remoto, datos disponibles y
fallo de consulta. La carga tiene tiempo y tamaño máximos. Si GitHub no
responde, el diff local sigue disponible. También presenta el upstream local
y los commits por delante o detrás cuando Git los conoce.

## Calendario de contribuciones

Configuración → GitHub muestra las contribuciones de la cuenta verificada
durante el último año. Usa `HeatmapChart` de `openide-ui`, con meses, intensidad
por día, total, tooltip y leyenda. Un día sin actividad conserva su celda;
los días fuera del período quedan vacíos. La grilla se ajusta al ancho de la vista.

La primera visita inicia una consulta GraphQL en un worker independiente.
La navegación conserva el resultado; el botón de actualización permite volver
a consultarlo. Una reconexión invalida los datos y las respuestas pendientes.
Cada respuesta se asocia a la cuenta y a una generación para evitar mostrar
datos de otra cuenta o de una consulta anterior.

La credencial se resuelve para la cuenta seleccionada y permanece en el worker.
Se verifican la identidad del `viewer`, las fechas, los días de semana,
la continuidad del calendario y los límites de tamaño y duración. Los errores
GraphQL, incluso con HTTP 200, muestran un estado de error con reintento.
No se generan datos de ejemplo cuando falta acceso.

El calendario puede empezar hasta seis días antes del período consultado,
porque GitHub completa la primera semana desde el domingo. Ese margen se
incluye en la validación junto al año de hasta 366 días; la continuidad de
fechas y semanas sigue siendo obligatoria.

Los campos y el período provienen de la
[API GraphQL de GitHub](https://docs.github.com/en/graphql/reference/users#contributioncalendar).
Las contribuciones privadas dependen del acceso de la credencial y de la
configuración de la cuenta. No se solicitan permisos adicionales al abrir el gráfico.

## Alcance pendiente

- Crear, comentar, aprobar y fusionar PR requiere un flujo de autenticación y
  permisos de escritura propios, con estados de confirmación y errores.
- Faltan remotos GitHub Enterprise y casos de fork con nombres de remotos no
  convencionales.
- Check runs, reviews, conflictos y conversaciones de PR aún no se presentan.
- GitHub Enterprise requiere un host y endpoint configurables.

La lectura de PR usa el [endpoint oficial de GitHub](https://docs.github.com/en/rest/pulls/pulls).
