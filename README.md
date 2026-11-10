# Partir Excel

Aplicación de escritorio en Rust para Windows de 64 bits. Divide un libro en fragmentos de tamaño configurable y reconstruye el archivo original. Funciona localmente, sin instalar Excel.

[Descargar PartirExcel.exe para Windows de 64 bits](https://github.com/nelfel/partir-excel/releases/latest/download/PartirExcel.exe)

## Enviar un libro

1. Abre **Partir Excel** y selecciona **Dividir**.
2. Elige el Excel o arrástralo a la ventana. Indica el máximo por parte. Puedes usar una coma decimal, por ejemplo **2,5 MB**.
3. Revisa el número de partes, elige el destino y pulsa **Dividir archivo**.
4. Abre la carpeta de resultado. Envía todos los archivos `parte-….bin` y el índice `Unir.json`. Conserva sus nombres y comparte la aplicación con quien vaya a reconstruir el libro.

## Recibir el libro

1. Guarda las partes y el índice juntos en una carpeta.
2. Abre la app, pulsa **Unir** y elige `Unir.json`. También puedes arrastrar el índice directamente a la ventana.
3. Elige el destino y pulsa **Reconstruir Excel**. Al terminar, abre la carpeta de resultado y encontrarás el libro con su extensión original.

La app comprueba la huella SHA-256 de cada parte y del archivo completo. Si una parte falta o está dañada, el mensaje te indica cuál debes volver a copiar. Si ya existe el nombre de destino, la copia nueva recibe un número entre paréntesis.

## Tamaños

Con **MB**, cada unidad equivale a 1 000 000 bytes. Un archivo de 180 000 000 bytes produce nueve partes de 20 MB, además del pequeño índice. Uno de 47 MB produce dos partes de 20 MB y una de 7 MB. El selector **MiB** usa unidades de 1 048 576 bytes. El contador muestra el resultado para el archivo elegido antes de empezar.

Los fragmentos sirven para transporte. Abre el Excel después de unirlos. Al copiar los bytes se conservan las hojas, las fórmulas, el formato y las macros del libro. Se admiten las extensiones `.xlsx`, `.xlsm`, `.xlsb`, `.xls`, `.xltx`, `.xltm` y `.xlt`.

## Compilar desde el código

En Windows con Rust y su enlazador instalados, ejecuta:

```sh
cargo test --release --locked
cargo build --release --locked
```

El ejecutable queda en `target/release/partir-excel.exe`. Las pruebas incluyen el recorrido de dividir y reconstruir desde los botones, nueve partes de 20 MB, la última parte más pequeña, archivos vacíos, cancelación, daños y colisiones de nombres.
