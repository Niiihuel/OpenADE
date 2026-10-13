# Idioma de la interfaz

openADE se escribe en español: los textos están en el código tal cual. Con
**Ajustes → Apariencia → English** activado, lo que la ventana va a dibujar
se pasa al inglés justo antes de dibujarse. Vale para las dos ventanas
(agentes e IDE), que son un mismo proceso, y se aplica desde el cuadro
siguiente, sin reiniciar. La elección se guarda en el archivo `language` de la
carpeta de datos.

## Cómo funciona

- `apps/agents-window/src/i18n.rs` recorre el árbol de elementos del cuadro y
  reemplaza cada texto por su entrada del catálogo. Las filas de las listas
  virtuales se traducen cuando se construyen.
- El catálogo es `apps/agents-window/assets/i18n/en.tsv`: español, tabulador,
  inglés, una entrada por línea, con `\n`, `\t` y `\\` escritos así.
- Un texto con valores (`"Cada {day} a las {hour:02}:{minute:02}"`) se
  reconoce por sus tramos fijos, se leen los valores y se arma el inglés con
  los mismos. Un valor que es a su vez una palabra de la interfaz (`lunes`)
  también se traduce.
- limn cambia sus propios textos con `set_labels`.

## Lo que no se traduce

- Lo que no tiene entrada en el catálogo se ve en español.
- El contenido: lo que hay en un editor, un diff, el valor de un campo.
- Lo que se le envía al modelo y lo que el modelo responde.
- Las etiquetas dentro de los gráficos.

Un texto ajeno que coincide exactamente con una etiqueta (un archivo llamado
`archivo`) se muestra traducido: el catálogo no puede distinguirlo.

## Agregar o corregir textos

Una cadena nueva en el código se ve en español en ambos idiomas hasta que
tenga su línea en el catálogo. `tools/i18n/extract.py <salida.json>` lista las
cadenas en español del código (sin pruebas); las que falten en el catálogo son
las que hay que agregar. La prueba
`every_entry_of_the_catalog_keeps_the_values_of_its_text` falla si una entrada
pierde o cambia un valor entre llaves.
