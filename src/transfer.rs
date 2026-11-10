use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use tempfile::{Builder, NamedTempFile};

pub const EXTENSIONS: &[&str] = &["xlsx", "xlsm", "xlsb", "xls", "xltx", "xltm", "xlt"];
pub const INDEX: &str = "Unir.json";
const MAX_PARTS: u64 = 100_000;
const MAX_INDEX_BYTES: u64 = 12_000_000;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub fn size_limit(input: &str, unit: u64) -> io::Result<u64> {
    let text = input.trim().replace(',', ".");
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let bad_size = || invalid("Escribe un tamaño positivo, por ejemplo 20 o 0,5.");
    if whole.is_empty()
        || fraction.len() > 6
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err(bad_size());
    }
    let whole = whole.parse::<u64>().map_err(|_| bad_size())?;
    let decimal = fraction.parse::<u64>().unwrap_or(0) * unit / 10u64.pow(fraction.len() as u32);
    whole
        .checked_mul(unit)
        .and_then(|b| b.checked_add(decimal))
        .filter(|b| *b > 0)
        .ok_or_else(bad_size)
}

pub fn part_count(size: u64, limit: u64) -> io::Result<u64> {
    if limit == 0 {
        return Err(invalid("El tamaño máximo debe ser positivo."));
    }
    let count = size.div_ceil(limit).max(1);
    if count > MAX_PARTS {
        return Err(invalid(
            "Se superarían las 100 000 partes. Aumenta el tamaño por parte.",
        ));
    }
    Ok(count)
}

fn part_name(index: u64) -> String {
    format!("parte-{index:04}.bin")
}

fn check_name(name: &str) -> io::Result<()> {
    let ext = Path::new(name)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    let device = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_uppercase();
    let reserved = matches!(
        device.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || device
        .strip_prefix("COM")
        .or_else(|| device.strip_prefix("LPT"))
        .is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
    if reserved
        || name.encode_utf16().count() > 255
        || name
            .chars()
            .any(|c| c.is_control() || r#"\/:"*?<>|"#.contains(c))
        || !EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
    {
        return Err(invalid(
            "El índice o la selección contiene un nombre de Excel no válido.",
        ));
    }
    Ok(())
}

pub(crate) fn open_input(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // Other readers are allowed, writers cannot change the input mid-copy.
    }
    options.open(path)
}

#[derive(Serialize, Deserialize)]
pub struct Index {
    version: u8,
    pub archivo: String,
    pub bytes: u64,
    bytes_por_parte: u64,
    sha256: String,
    pub partes: Vec<String>,
}

pub fn read_index(path: &Path) -> io::Result<Index> {
    let mut data = Vec::new();
    open_input(path)?
        .take(MAX_INDEX_BYTES + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_INDEX_BYTES {
        return Err(invalid("El índice es demasiado grande."));
    }
    let index: Index = serde_json::from_slice(&data)
        .map_err(|_| invalid("Selecciona el archivo Unir.json generado al dividir el Excel."))?;
    check_name(&index.archivo)?;
    let valid_hash = |v: &str| {
        v.len() == 64
            && v.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    };
    if index.version != 1
        || part_count(index.bytes, index.bytes_por_parte)? != index.partes.len() as u64
        || !valid_hash(&index.sha256)
        || !index.partes.iter().all(|h| valid_hash(h))
    {
        return Err(invalid(
            "El índice está incompleto o tiene un formato incompatible.",
        ));
    }
    Ok(index)
}

pub struct Outcome {
    pub path: PathBuf,
    pub parts: u64,
}

pub(crate) fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Operación cancelada.",
        ))
    } else {
        Ok(())
    }
}

// One buffer and two running hashes cover both directions, regardless of workbook size.
struct Copier<'a, F> {
    buffer: Vec<u8>,
    whole: Sha256,
    done: u64,
    total: u64,
    cancel: &'a AtomicBool,
    progress: F,
}

impl<'a, F: FnMut(u64, u64)> Copier<'a, F> {
    fn new(total: u64, cancel: &'a AtomicBool, progress: F) -> Self {
        Self {
            buffer: vec![0; 512 * 1024],
            whole: Sha256::new(),
            done: 0,
            total,
            cancel,
            progress,
        }
    }

    fn part(
        &mut self,
        source: &mut File,
        output: &mut File,
        mut remaining: u64,
    ) -> io::Result<String> {
        let mut digest = Sha256::new();
        check_cancel(self.cancel)?;
        while remaining > 0 {
            check_cancel(self.cancel)?;
            let amount = remaining.min(self.buffer.len() as u64) as usize;
            let bytes = &mut self.buffer[..amount];
            source.read_exact(bytes)?;
            output.write_all(bytes)?;
            self.whole.update(&*bytes);
            digest.update(&*bytes);
            self.done += amount as u64;
            remaining -= amount as u64;
            (self.progress)(self.done, self.total);
        }
        Ok(format!("{:x}", digest.finalize()))
    }
}

pub fn split(
    input: &Path,
    dest: &Path,
    limit: u64,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> io::Result<Outcome> {
    let name = input
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| invalid("Nombre de archivo no válido."))?;
    check_name(name)?;
    let mut source = open_input(input)?;
    let total = source.metadata()?.len();
    let parts = part_count(total, limit)?;
    let stem: String = input
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .chars()
        .take(48)
        .collect();
    let dir = Builder::new()
        .prefix(&format!("{stem} - partes-"))
        .tempdir_in(dest)?;
    let mut copier = Copier::new(total, cancel, progress);
    let mut hashes = Vec::with_capacity(parts as usize);
    for number in 1..=parts {
        let mut output = File::create_new(dir.path().join(part_name(number)))?;
        hashes.push(copier.part(&mut source, &mut output, limit.min(total - copier.done))?);
        output.sync_all()?;
    }
    if source.read(&mut [0])? != 0 {
        return Err(invalid("El tamaño del archivo cambió durante la lectura."));
    }
    let index = Index {
        version: 1,
        archivo: name.into(),
        bytes: total,
        bytes_por_parte: limit,
        sha256: format!("{:x}", copier.whole.finalize()),
        partes: hashes,
    };
    let mut file = File::create_new(dir.path().join(INDEX))?;
    file.write_all(&serde_json::to_vec_pretty(&index)?)?;
    file.sync_all()?;
    drop(file);
    check_cancel(cancel)?;
    Ok(Outcome {
        path: dir.keep(),
        parts,
    })
}

pub fn join(
    index_path: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> io::Result<Outcome> {
    let index = read_index(index_path)?;
    let folder = index_path
        .parent()
        .ok_or_else(|| invalid("No se encontró la carpeta de las partes."))?;
    let mut temp = NamedTempFile::new_in(dest)?;
    let mut copier = Copier::new(index.bytes, cancel, progress);
    for (i, expected_hash) in index.partes.iter().enumerate() {
        check_cancel(cancel)?;
        let name = part_name(i as u64 + 1);
        let mut input = open_input(&folder.join(&name))
            .map_err(|e| io::Error::new(e.kind(), format!("No se pudo abrir {name}. {e}")))?;
        let expected = index.bytes_por_parte.min(index.bytes - copier.done);
        if input.metadata()?.len() != expected {
            return Err(invalid(format!(
                "El tamaño de {name} es incorrecto. Vuelve a copiar esa parte."
            )));
        }
        let actual = copier.part(&mut input, temp.as_file_mut(), expected)?;
        if &actual != expected_hash {
            return Err(invalid(format!(
                "{name} está dañada o pertenece a otro envío."
            )));
        }
    }
    if format!("{:x}", copier.whole.finalize()) != index.sha256 {
        return Err(invalid(
            "El archivo reconstruido no coincide con la huella del original.",
        ));
    }
    temp.as_file().sync_all()?;
    let filename = Path::new(&index.archivo);
    let stem = filename.file_stem().unwrap().to_string_lossy();
    let ext = filename.extension().unwrap().to_string_lossy();
    for copy in 1u64.. {
        check_cancel(cancel)?;
        let path = if copy == 1 {
            dest.join(filename)
        } else {
            // Leave room under the Windows component limit when adding a copy number.
            let suffix = format!(" ({copy}).{ext}");
            let mut room = 255 - suffix.encode_utf16().count();
            let shortened: String = stem
                .chars()
                .take_while(|c| {
                    if c.len_utf16() > room {
                        false
                    } else {
                        room -= c.len_utf16();
                        true
                    }
                })
                .collect();
            dest.join(format!("{shortened}{suffix}"))
        };
        match temp.persist_noclobber(&path) {
            Ok(_) => {
                return Ok(Outcome {
                    path,
                    parts: index.partes.len() as u64,
                });
            }
            Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => temp = e.file,
            Err(e) => return Err(e.error),
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn limits_and_names() {
        assert_eq!(size_limit(" 0,5 ", 1_000_000).unwrap(), 500_000);
        assert_eq!(size_limit("20", 1_048_576).unwrap(), 20 * 1_048_576);
        for text in ["", "0", "-2", "NaN", "1.2.3", "1e4", "99999999999999999999"] {
            assert!(size_limit(text, 1_000_000).is_err());
        }
        assert!(part_count(u64::MAX, 1).is_err());
        for file in [
            "../evil.xlsx",
            r"C:\evil.xlsx",
            "book.xlsx:evil.xlsx",
            "NUL.xlsx",
            "wrong.exe",
        ] {
            assert!(check_name(file).is_err());
        }
    }

    #[test]
    fn short_last_part_empty_files_and_collisions() -> io::Result<()> {
        let stop = AtomicBool::new(false);
        for (input, lengths) in [
            (b"abcdefghij".as_slice(), vec![4, 4, 2]),
            (b"", vec![0]),
            (b"x", vec![1]),
        ] {
            let dir = tempdir()?;
            let book = dir.path().join("Año y región.xlsm");
            fs::write(&book, input)?;
            let bundle = split(&book, dir.path(), 4, &stop, |_, _| {})?;
            for (i, size) in lengths.into_iter().enumerate() {
                assert_eq!(
                    fs::metadata(bundle.path.join(part_name(i as u64 + 1)))?.len(),
                    size
                );
            }
            let result = join(&bundle.path.join(INDEX), dir.path(), &stop, |_, _| {})?;
            assert_eq!(result.path.file_name().unwrap(), "Año y región (2).xlsm");
            assert_eq!(fs::read(result.path)?, input);
            assert_eq!(fs::read(book)?, input);
        }
        Ok(())
    }

    #[test]
    fn invalid_or_missing_part_never_publishes_a_workbook() -> io::Result<()> {
        let stop = AtomicBool::new(false);
        let dir = tempdir()?;
        let dest = tempdir()?;
        let book = dir.path().join("Notas.xlsx");
        fs::write(&book, b"abcdefghij")?;
        let bundle = split(&book, dir.path(), 4, &stop, |_, _| {})?;
        let second = bundle.path.join(part_name(2));
        let index = bundle.path.join(INDEX);
        for content in [b"FAIL".as_slice(), b"?", b""] {
            if content.is_empty() {
                fs::remove_file(&second)?;
            } else {
                fs::write(&second, content)?;
            }
            let err = join(&index, dest.path(), &stop, |_, _| {}).err().unwrap();
            assert!(err.to_string().contains("parte-0002.bin"));
            assert_eq!(fs::read_dir(dest.path())?.count(), 0);
        }
        fs::write(second, b"efgh")?;
        let mut manifest = read_index(&index)?;
        manifest.sha256 = "0".repeat(64);
        fs::write(&index, serde_json::to_vec(&manifest)?)?;
        assert!(join(&index, dest.path(), &stop, |_, _| {}).is_err());
        assert_eq!(fs::read_dir(dest.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn cancellation_cleans_both_operations() -> io::Result<()> {
        let stop = AtomicBool::new(false);
        let dir = tempdir()?;
        let dest = tempdir()?;
        let book = dir.path().join("Uno.xls");
        fs::write(&book, [42u8; 100])?;
        let cancel = |_, _| stop.store(true, Ordering::Relaxed);
        assert_eq!(
            split(&book, dest.path(), 20, &stop, cancel)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(fs::read_dir(dest.path())?.count(), 0);
        stop.store(false, Ordering::Relaxed);
        let bundle = split(&book, dir.path(), 20, &stop, |_, _| {})?;
        assert_eq!(
            join(&bundle.path.join(INDEX), dest.path(), &stop, cancel)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(fs::read_dir(dest.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn nine_parts_of_twenty_decimal_mb() -> io::Result<()> {
        let dir = tempdir()?;
        let book = dir.path().join("Carga.xlsx");
        let block: Vec<u8> = (0..1_000_000u32)
            .map(|i| (i.wrapping_mul(7919) >> 5) as u8)
            .collect();
        let mut src = File::create(&book)?;
        for index in 0..180u8 {
            src.write_all(&[index])?;
            src.write_all(&block[1..])?;
        }
        drop(src);
        let stop = AtomicBool::new(false);
        let bundle = split(&book, dir.path(), 20_000_000, &stop, |_, _| {})?;
        assert_eq!(bundle.parts, 9);
        for i in 1..=9 {
            assert_eq!(
                fs::metadata(bundle.path.join(part_name(i)))?.len(),
                20_000_000
            );
        }
        let restored = join(
            &bundle.path.join(INDEX),
            bundle.path.as_path(),
            &stop,
            |_, _| {},
        )?;
        let mut original = File::open(book)?;
        let mut result = File::open(restored.path)?;
        let (mut a, mut b) = (vec![0u8; 1_000_000], vec![0u8; 1_000_000]);
        for _ in 0..180 {
            original.read_exact(&mut a)?;
            result.read_exact(&mut b)?;
            assert_eq!(a.as_slice(), b.as_slice());
        }
        assert_eq!(result.read(&mut [0])?, 0);
        Ok(())
    }
}
