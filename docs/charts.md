# Charts nativos

`openide-ui` incluye `HeatmapChart`, `BarChart`, `ColumnChart` y `ChartLegend`. Implementan
`Component::render()` y construyen elementos ordinarios: layout, recorte,
colores y tooltips pasan por `UiTree` y los comandos de dibujo existentes.
No agregan dependencias, un navegador ni un renderer separado.

El host suministra los datos y conserva los estados de carga y error. Al
redimensionar la ventana debe pasar el nuevo ancho disponible a `.width(...)`.
El componente no consulta servicios ni conserva copias independientes de los
datos del dominio.

## Heatmap

`HeatmapChart` recibe columnas de `Vec<Option<HeatmapCell>>`; un calendario
usa una columna por semana y siete posiciones de domingo a sábado.
`None` deja un hueco para días fuera del período. El nivel cero representa
un día real sin actividad. `HeatmapScale` contiene cinco colores; los niveles
se limitan a 0–4. Cada celda tiene un ID estable y un texto de ayuda.

`ChartAxisLabel` coloca un rótulo sobre una columna. Los rótulos se espacian
para evitar superposiciones; al reducir el ancho, la grilla conserva todas
las celdas y omite las etiquetas secundarias que no caben. La escala y los
textos se pueden adaptar a actividad, uso de herramientas u otras matrices.

```rust
use openide_ui::{Component, ComponentPalette, HeatmapCell, HeatmapChart};

let chart = HeatmapChart::new(
    "activity",
    vec![vec![
        None,
        Some(HeatmapCell::new("2026-09-28", 2, "3 contribuciones · 28 de septiembre")),
    ]],
    ComponentPalette::neutral_dark(),
)
.width(640.0)
.render();
```

## Barras

`BarChart` recibe `ChartDatum` con ID, etiqueta y valor. Las barras
horizontales comparten una escala lineal desde cero hasta el mayor valor.
Los valores y etiquetas son visibles sin hover. Se pueden configurar el color,
el texto del valor y el tooltip por observación. Valores negativos o no finitos
se representan como cero; una serie de ceros conserva geometría válida.

```rust
use openide_ui::{BarChart, ChartDatum, Component, ComponentPalette};

let chart = BarChart::new(
    "tasks",
    vec![ChartDatum::new("native", "Agente Rust", 8.0)],
    ComponentPalette::neutral_dark(),
)
.width(640.0)
.render();
```

### Columnas comparativas

`BarChart::vertical()` crea un `ColumnChart` con los mismos datos, color y ancho.
También se puede construir directamente con `ColumnChart::new`. Su eje numérico
parte de cero, usa intervalos legibles 1–2–5 e incluye etiquetas enteras por defecto.
Cada categoría muestra su nombre y cantidad; el tooltip conserva el nombre
completo cuando el espacio requiere abreviar el rótulo.

```rust
let chart = BarChart::new(
    "tasks",
    vec![
        ChartDatum::new("native", "Agente nativo", 8.0),
        ChartDatum::new("cli", "Agente CLI", 3.0),
    ],
    ComponentPalette::neutral_dark(),
)
.width(640.0)
.vertical()
.height(240.0)
.max_bar_width(48.0)
.integer_ticks(true)
.render();
```

Las columnas tienen extremos superiores redondeados y un ancho máximo de 48 px;
una sola categoría queda centrada en el área del gráfico. Un valor cero conserva
su etiqueta y tooltip, sin dibujar una barra de altura ficticia. Si las categorías
no caben, sólo el área del gráfico tiene scroll horizontal; el eje numérico queda
fijo. Si caben, no se crea un contenedor que capture la rueda de la página.

Los colores proceden de `ComponentPalette` y del color opcional de cada dato. No
hay un bucle de animación continuo ni un renderer adicional. El diseño toma como
referencia [Basic Bar de Apache ECharts](https://echarts.apache.org/handbook/en/how-to/chart-types/bar/basic-bar/):
categorías discretas, escala común, separación y ancho máximo de columnas.

Los tooltips de gráficos son regiones `HitKind::Hover`. No emiten acciones de
clic ni agregan paradas al recorrido de Tab. Los botones existentes conservan
sus tooltips de hover y foco de teclado.

## Uso en la aplicación

- **Configuración → GitHub:** calendario de la cuenta verificada con meses,
  colores verdes, total anual, detalle por día, leyenda y actualización manual.
  Se consulta al visitar la sección y el resultado se conserva al navegar.
  La conexión y los errores se presentan con los estados compartidos de Settings.
- **Configuración → Uso:** tareas por agente, calculadas a partir del historial
  visible en el historial. Muestra el total exacto, los nombres configurados y
  columnas ordenadas por cantidad. Si no hay tareas, muestra cero y un estado vacío.

El contrato público está en `crates/openide-ui/src/charts.rs`; la adaptación
de los datos del dominio está en
`apps/agents-window/src/settings_ui/activity_charts.rs`. El calendario de
GitHub se valida y carga en `github_contributions.rs`, fuera del hilo visual.

ZCode usa un contenedor de Recharts en
`references/zcode/packages/ui/src/components/ui/chart.tsx`. Se consultó como
referencia de presentación y separación de datos, tema y tooltips. Estos
componentes Rust son una implementación propia con el motor de OpenIDE.
