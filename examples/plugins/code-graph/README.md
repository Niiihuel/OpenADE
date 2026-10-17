# Grafo de código

Plugin que da al agente nativo un índice del proyecto: definiciones con sus líneas, quién llama a quién e imports entre archivos. El agente pregunta en vez de abrir y grepear archivos completos, y gasta muchos menos tokens.

Requiere `openade-graph` en el PATH:

```sh
cargo install --path crates/openide-graph
```

En **Configuración → Plugins** selecciona esta carpeta, confirma la instalación y actívala para el proyecto. En un turno **Construir** el agente tendrá:

| Herramienta | Para qué |
|---|---|
| `mcp__plugin_code-graph__graph_outline` | Definiciones de un archivo con sus rangos de líneas, imports, quién lo importa y copias idénticas. Se usa antes de leer, para leer sólo el rango necesario. |
| `mcp__plugin_code-graph__graph_find` | Ubicar una definición por nombre o por nombre calificado (`Graph::refresh`). |
| `mcp__plugin_code-graph__graph_explain` | Una definición con su contenedor, miembros, llamadores, llamadas e implementaciones. |
| `mcp__plugin_code-graph__graph_path` | La cadena de llamadas más corta entre dos definiciones. |
| `mcp__plugin_code-graph__graph_stats` | Tamaño del índice por lenguaje. |

## Cómo se mantiene al día

El índice vive en `~/.cache/openade/graph/<hash del proyecto>.db` (SQLite en modo WAL) y es el mismo para todos los procesos que abren el proyecto. Antes de cada consulta se revisa el tamaño y la fecha de cada archivo, y sólo se re-parsean los que cambiaron de contenido. En openADE eso tarda unos 10 ms; el primer indexado, unos 3 s. Se respeta `.gitignore` y se saltean archivos de más de 4 MiB.

| Formato | Qué se indexa |
|---|---|
| Rust, Python, JavaScript, TypeScript/TSX, C, C++, Go, Java, Lua, Swift | funciones, métodos, tipos y módulos; llamadas; imports (resueltos a archivos donde son rutas) |
| QML | cada archivo como componente con su nombre, propiedades, señales y funciones; usos de componentes (`Pill { }`) |
| Nix | bloques de atributos (`programs.kitty = { .. }`) y funciones; rutas (`./hardware.nix`, `./kitty` → `default.nix`) |
| Bash | funciones, comandos llamados y `source` |
| SQL | tablas, vistas, funciones y tipos; tablas usadas en consultas |
| Markdown | títulos anidados como secciones; enlaces relativos y `[[wikilinks]]` (por nombre en todo el proyecto, como Obsidian) |
| TOML | tablas y sus claves; `path = ".."` y `members` de Cargo enlazan con el `Cargo.toml` destino |
| YAML | claves anidadas |
| JSON | dos niveles de claves y `extends`; se saltean los `*lock.json` |

## Límites

Las llamadas se resuelven **por nombre**, sin tipos: un llamado a `new` se asocia con todas las definiciones llamadas `new`. `graph_explain` avisa cuando varias definiciones comparten nombre, y `graph_path` marca con `[guess]` cada salto ambiguo.

## Hook: consultar el grafo antes de leer o buscar

`openade-graph hook` responde eventos `PreToolUse` de Claude Code. openADE lee los mismos hooks de `~/.claude/settings.json`, así que un registro sirve para ambos:

```json
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Read|Grep|Bash", "hooks": [{ "type": "command", "command": "openade-graph hook", "timeout": 10 }] }
    ]
  }
}
```

- Sólo actúa en proyectos que ya tienen grafo (`openade-graph index` en la raíz, o el plugin activo). En cualquier otro lado no dice nada, y nunca indexa por su cuenta.
- Un `Read` del archivo entero de más de 400 líneas se rechaza y el agente recibe el outline, con la indicación de volver a leer con `offset` y `limit`. Pasar `offset: 1` lo lee entero a propósito. El `read_file` de openADE no acepta rangos, así que nunca se rechaza.
- Un `Grep` o un `rg`/`grep` en `Bash` que busca un identificador conocido sigue adelante, y el agente recibe además dónde está definido y la sugerencia de usar `graph_explain`.
- Ante cualquier error, el hook no imprime nada: nunca bloquea una herramienta por fallar.

Codex usa la misma estructura en `.codex/hooks.json` (hay que aprobarlo con `/hooks`); sus nombres de herramientas difieren y no está probado ahí.

## Usarlo desde otros CLIs

El mismo binario sirve MCP por stdio y comparte el índice:

```sh
claude mcp add code-graph -- openade-graph mcp          # Claude Code, desde la raíz del proyecto
openade-graph --workspace . explain refresh             # a mano o desde scripts
```
