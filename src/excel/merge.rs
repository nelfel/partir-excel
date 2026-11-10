use super::{Book, Result, check_cancel, open_input, package, reader, rows};
use crate::transfer::{Outcome, persist_result};
use quick_xml::events::Event;
use std::{
    collections::HashSet,
    fs::File,
    io::{self, BufReader, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use zip::ZipArchive;

#[derive(PartialEq)]
struct Part {
    path: PathBuf,
    stem: String,
    sheet: String,
    sheet_num: usize,
    number: usize,
    total: Option<usize>,
    batch: Option<String>,
}

fn batch_id(zip: &mut ZipArchive<File>) -> Result<Option<String>> {
    let core = match zip.by_name("docProps/core.xml") {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut xml = reader(core);
    let mut buf = Vec::new();
    let mut identifier = false;
    loop {
        match xml.read_event_into(&mut buf)? {
            Event::Start(tag) => identifier = tag.local_name().as_ref() == "identifier",
            Event::End(_) => identifier = false,
            Event::Text(t) if identifier => {
                if let Some(id) = t.as_ref().strip_prefix("PartirExcel:") {
                    if id.len() != 16 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
                        return Err("La identificación del conjunto de partes está dañada.".into());
                    }
                    return Ok(Some(id.to_owned()));
                }
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
        buf.clear();
    }
}

impl Part {
    fn read(path: &Path, zip: &mut ZipArchive<File>, book: &Book) -> Result<Self> {
        let name_error =
            "Elige archivos .xlsx creados con Dividir en Excel y conserva sus nombres originales.";
        let [(sheet, _)] = book.sheets.as_slice() else {
            return Err(name_error.into());
        };
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))
        {
            return Err(name_error.into());
        }
        let file = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or(name_error)?;
        let (prefix, suffix) = file.rsplit_once(" - ").ok_or(name_error)?;
        let clipped = prefix
            .strip_suffix(&format!(" {}", package::safe_sheet_name(sheet)))
            .ok_or(name_error)?;
        let (stem, sheet_num) = clipped.rsplit_once(" - ").ok_or(name_error)?;
        let positive = |s: &str| -> Result<usize> {
            s.parse::<usize>()
                .ok()
                .filter(|n| *n > 0 && s.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(|| name_error.into())
        };
        let (number, total) = match suffix.split_once(" de ") {
            Some((a, b)) => (positive(a)?, Some(positive(b)?)),
            None => (positive(suffix)?, None),
        };
        let batch = batch_id(zip)?;
        if batch.is_some() != total.is_some() || stem.is_empty() {
            return Err(name_error.into());
        }
        Ok(Self {
            path: path.to_owned(),
            stem: stem.to_owned(),
            sheet: sheet.clone(),
            sheet_num: positive(sheet_num)?,
            number,
            total,
            batch,
        })
    }
}

pub fn join(
    paths: &[PathBuf],
    dest: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> io::Result<Outcome> {
    merge(paths, dest, cancel, progress).map_err(io::Error::other)
}

fn merge(
    paths: &[PathBuf],
    dest: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<Outcome> {
    let mut parts: Vec<Part> = Vec::new();
    let mut format: Option<Book> = None;
    for path in paths {
        check_cancel(cancel)?;
        let mut zip = ZipArchive::new(open_input(path)?)?;
        let mut book = Book::read(&mut zip, cancel)?;
        let part = Part::read(path, &mut zip, &book)?;
        if let Some(first) = parts.first() {
            if first.stem != part.stem || first.batch != part.batch || first.total != part.total {
                return Err("Las partes seleccionadas pertenecen a distintos envíos. Elige un conjunto completo.".into());
            }
        }
        if let Some(first) = &format {
            if !same_format(first, &book) {
                return Err("Las partes tienen formatos incompatibles. Usa las partes originales de una misma división.".into());
            }
        } else {
            book.strings = Vec::new();
            format = Some(book);
        }
        parts.push(part);
    }
    let format = format.ok_or("Selecciona todas las partes .xlsx de un libro.")?;
    parts.sort_by_key(|p| (p.sheet_num, p.number));
    let first = &parts[0];
    if let Some(total) = first.total {
        if total != parts.len() {
            return Err(format!("Este envío tiene {total} partes y seleccionaste {}. Selecciona todas una sola vez.", parts.len()).into());
        }
    }
    let mut sheets = Vec::new();
    let mut names = HashSet::new();
    let mut prior = 0;
    for group in parts.chunk_by(|a, b| a.sheet_num == b.sheet_num) {
        let name = &group[0].sheet;
        if !names.insert(name.to_lowercase()) {
            return Err("Dos hojas distintas tienen el mismo nombre.".into());
        }
        sheets.push(name.as_str());
        for (i, part) in group.iter().enumerate() {
            let expected = if first.total.is_some() {
                prior + i + 1
            } else {
                i + 1
            };
            if part.number != expected || &part.sheet != name {
                return Err(format!("Revisa las partes de «{name}». Se esperaba la parte {expected} y se encontró la {} de «{}».", part.number, part.sheet).into());
            }
        }
        prior += group.len();
    }
    let template = package::template(&format, &sheets, None)?;
    let mut temp = tempfile::NamedTempFile::new_in(dest)?;
    let mut writer = template.start(temp.as_file_mut())?;
    let mut row_buf = package::RowBuffer::default();
    let mut done = 0;
    let total = parts.len() as u64 * 1000;
    for (sheet_i, group) in parts
        .chunk_by(|a, b| a.sheet_num == b.sheet_num)
        .enumerate()
    {
        let mut header = Vec::new();
        let mut columns = String::new();
        let mut out_row = 0;
        for (part_i, part) in group.iter().enumerate() {
            check_cancel(cancel)?;
            let mut zip = ZipArchive::new(open_input(&part.path)?)?;
            let book = Book::read(&mut zip, cancel)?;
            if Part::read(&part.path, &mut zip, &book)? != *part || !same_format(&format, &book) {
                return Err(
                    "Una parte cambió durante la unión. Vuelve a seleccionar los archivos.".into(),
                );
            }
            let source = zip.by_name(&book.sheets[0].1)?;
            let bytes = source.size().max(1);
            let rows = rows::scan(source, &book.strings, dest, cancel, |pos| {
                progress(done + pos.min(bytes) * 500 / bytes, total)
            })?;
            if rows.len() == 0 {
                return Err(format!("La parte {} no tiene encabezado.", part.number).into());
            }
            let start = usize::from(part_i > 0);
            if out_row + rows.len() - start > 1_048_576 {
                return Err(format!(
                    "La hoja «{}» supera el máximo de filas de Excel.",
                    part.sheet
                )
                .into());
            }
            let mut source = BufReader::with_capacity(256 * 1024, rows.file);
            source.rewind()?;
            let mut current_header = vec![0; usize::try_from(rows.offsets[1])?];
            source.read_exact(&mut current_header)?;
            if part_i == 0 {
                header = current_header;
                columns = rows.columns;
                template.start_sheet(&mut writer, sheet_i + 1, &columns)?;
                source.rewind()?;
            } else if current_header != header || rows.columns != columns {
                return Err(format!("El encabezado o las columnas de la parte {} de «{}» no coinciden con la primera parte.", part.number, part.sheet).into());
            }
            for (i, pair) in rows.offsets[start..].windows(2).enumerate() {
                check_cancel(cancel)?;
                out_row += 1;
                row_buf.write(&mut source, &mut writer, pair[1] - pair[0], out_row)?;
                if i % 128 == 0 {
                    progress(
                        done + 500 + i as u64 * 500 / (rows.offsets.len() - 1) as u64,
                        total,
                    );
                }
            }
            done += 1000;
            progress(done, total);
        }
        writer.write_all(b"</sheetData></worksheet>")?;
    }
    writer.finish()?;
    Ok(persist_result(
        temp,
        dest,
        &format!("{} - unido.xlsx", first.stem),
        parts.len() as u64,
        cancel,
    )?)
}

fn same_format(a: &Book, b: &Book) -> bool {
    a.styles == b.styles && a.theme == b.theme && a.date1904 == b.date1904 && a.strict == b.strict
}
