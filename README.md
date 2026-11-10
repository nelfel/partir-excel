# Partir Excel

Aplicación de escritorio en Rust para Windows de 64 bits. Divide las filas de un libro en archivos `.xlsx` que puedes enviar por correo y vuelve a reunirlos en un solo Excel. Funciona localmente, sin instalar Excel.

[Descargar PartirExcel.exe para Windows de 64 bits](https://github.com/nelfel/partir-excel/releases/latest/download/PartirExcel.exe)

## Enviar archivos Excel independientes

1. Abre **Partir Excel**. La opción inicial es **Dividir en Excel**.
2. Elige el libro original o arrástralo a la ventana. El tamaño máximo inicial es **15 MB**. Puedes ajustarlo a tu correo y usar decimales, por ejemplo **2,5 MB**.
3. Pulsa **Crear archivos .xlsx** y abre la carpeta de resultado. Cada archivo lleva el nombre de la hoja y un número de parte. El destinatario puede abrir cada adjunto directamente en Excel.

La app recorre las hojas visibles y repite la primera fila con datos como encabezado de cada parte. Conserva el orden de los registros, las columnas, los tipos de celda, los formatos numéricos y las fechas. Cada archivo respeta el máximo elegido, medido después de comprimir y cerrar el libro. La cantidad final depende de los datos, los encabezados y el número de hojas. Si una sola fila junto con su encabezado supera el máximo, la app pide aumentarlo.

Las partes contienen datos de las celdas. Las fórmulas se exportan con su último valor guardado. Si falta ese valor, la app indica la celda para que puedas recalcular y guardar el original en Excel. Se omiten las filas vacías, hojas ocultas, gráficos, macros y objetos como tablas dinámicas. Las filas con datos filtradas u ocultas también se exportan y conservan su atributo de visibilidad. Para este modo se admiten `.xlsx`, `.xlsm`, `.xltx` y `.xltm`. Para `.xls` o `.xlsb`, guarda primero una copia como `.xlsx` en Excel.

## Volver a unir las partes de Excel

1. Guarda todos los adjuntos `.xlsx` de una misma división con sus nombres originales.
2. Abre **Unir Excel**, pulsa **Elegir partes…** y selecciona todos esos archivos. También puedes arrastrarlos juntos a la ventana.
3. Pulsa **Unir en un .xlsx**. En la carpeta de destino encontrarás un libro llamado, por ejemplo, `Ventas - unido.xlsx`. Si el nombre ya existe, la app crea una copia numerada.

Puedes seleccionar las partes en cualquier orden. La app reconstruye las hojas exportadas con sus nombres, ordena las filas según su número de parte y escribe un solo encabezado por hoja. Conserva los datos que contienen las partes, incluidos los resultados guardados de las fórmulas.

Desde la versión 1.2, los nombres incluyen el número y el total de partes del envío. La app comprueba el conjunto antes de guardar. También admite las partes creadas con la versión 1.1. Estas solo indican el número dentro de cada hoja. Si las recibiste por correo, confirma con quien las envió cuántas partes hay por hoja. En ese formato, la app puede detectar un salto intermedio, pero carece del total para detectar una última parte o una hoja entera ausente.

## Enviar una copia exacta en fragmentos binarios

Selecciona **Crear .bin**, elige el libro y pulsa **Dividir archivo**. Envía todas las partes `.bin` y el índice `Unir.json`. Este modo también admite las partes creadas con la versión 1.0.

## Unir los fragmentos binarios

1. Guarda las partes y el índice juntos en una carpeta.
2. Abre la app, pulsa **Unir .bin** y elige `Unir.json`. También puedes arrastrar el índice directamente a la ventana.
3. Elige el destino y pulsa **Reconstruir Excel**. Al terminar, abre la carpeta de resultado y encontrarás el libro con su extensión original.

La app comprueba la huella SHA-256 de cada parte y del archivo completo. Si una parte falta o está dañada, el mensaje te indica cuál debes volver a copiar. Si ya existe el nombre de destino, la copia nueva recibe un número entre paréntesis.

## Tamaños

Con **MB**, cada unidad equivale a 1 000 000 bytes. El selector **MiB** usa unidades de 1 048 576 bytes. En el modo **Crear .bin**, un archivo de 180 000 000 bytes produce nueve partes de 20 MB, además del pequeño índice. Uno de 47 MB produce dos partes de 20 MB y una de 7 MB.

Los fragmentos sirven para transporte. Abre el Excel después de unirlos. Al copiar los bytes se conservan las hojas, las fórmulas, el formato y las macros del libro. Se admiten las extensiones `.xlsx`, `.xlsm`, `.xlsb`, `.xls`, `.xltx`, `.xltm` y `.xlt`.

## Compilar desde el código

En Windows con Rust y su enlazador instalados, ejecuta:

```sh
cargo test --release --locked
cargo build --release --locked
```

El ejecutable queda en `target/release/partir-excel.exe`. Las pruebas incluyen el recorrido de dividir y reconstruir desde los botones, nueve partes de 20 MB, la última parte más pequeña, archivos vacíos, cancelación, daños y colisiones de nombres.
