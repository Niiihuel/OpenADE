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
el worker usa `GH_TOKEN` o `GITHUB_TOKEN`, la sesión de GitHub CLI o la
credencial de Git Credential Manager, en ese orden. Los tokens no entran en las
vistas, sesiones ni configuración de OpenIDE.

El footer del sidebar consulta la identidad de forma asíncrona: valida tokens
del entorno o de `gh` con `/user`. `github list` sólo indica que GCM tiene
cuentas almacenadas; no demuestra que su token siga funcionando. Por eso el
footer muestra **Verificar GitHub** o **Elegir cuenta GitHub**, sin elegir la
primera cuenta si hay varias. La verificación con `get --no-ui` y `/user` ocurre
únicamente después de elegir una cuenta. Si falla, se ofrece reconectar.
«Conectar con GitHub» abre el flujo de navegador de Git Credential Manager
**sólo después del clic**; la consulta inicial no abre ventanas ni crea
credenciales. GCM conserva la autorización en el almacén del sistema. Tras
validar la identidad, el footer muestra el usuario y se actualiza la revisión
de GitHub activa. El botón del usuario abre su perfil; configuración es un
control independiente con hitbox y tooltip propios.

Para una revisión privada sin token del entorno ni sesión de `gh`, la lectura
automática de credenciales GCM se limita a una única cuenta almacenada. Con
varias cuentas no se toma una credencial arbitraria; el diff local y la lectura
pública siguen disponibles.
El avatar local usa la inicial del usuario junto al isotipo de GitHub; la
representación de fotos remotas requiere un componente de imagen reutilizable
en el renderer y queda fuera de esta integración de cuenta.

El dock muestra el repositorio y hasta cuatro PR abiertos para la rama actual,
con enlaces externos. Distingue carga, ausencia de remoto, datos disponibles y
fallo de consulta. La carga tiene tiempo y tamaño máximos. Si GitHub no
responde, el diff local sigue disponible. También presenta el upstream local
y los commits por delante o detrás cuando Git los conoce.

## Alcance pendiente

- Crear, comentar, aprobar y fusionar PR requiere un flujo de autenticación y
  permisos de escritura propios, con estados de confirmación y errores.
- Faltan remotos GitHub Enterprise y casos de fork con nombres de remotos no
  convencionales.
- Check runs, reviews, conflictos y conversaciones de PR aún no se presentan.
- GitHub Enterprise requiere un host y endpoint configurables.

La lectura de PR usa el [endpoint oficial de GitHub](https://docs.github.com/en/rest/pulls/pulls).
