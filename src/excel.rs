mod merge;
mod package;
mod rows;
pub use merge::join;

use crate::transfer::{Outcome, check_cancel, open_input};
use quick_xml::{
    Reader,
    escape::escape,
    events::{BytesStart, Event},
};
use std::{
    collections::HashMap,
    error::Error,
    fs::File,
    io::{self, BufReader, Read},
    path::Path,
    sync::atomic::AtomicBool,
};
use zip::ZipArchive;

pub const EXTENSIONS: &[&str] = &["xlsx", "xlsm", "xltx", "xltm"];
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const OFFICE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const TYPES: &str = "http://schemas.openxmlformats.org/package/2006/content-types";

fn reader(input: impl Read) -> Reader<BufReader<impl Read>> {
    let mut xml = Reader::from_reader(BufReader::with_capacity(128 * 1024, input));
    xml.config_mut().expand_empty_elements = true;
    xml
}

fn attribute(tag: &BytesStart, key: &str) -> Result<Option<String>> {
    tag.try_get_attribute(key)?
        .map(|a| Ok(quick_xml::escape::unescape(&a.value)?.into_owned()))
        .transpose()
}

fn required(tag: &BytesStart, key: &str) -> Result<String> {
    attribute(tag, key)?.ok_or_else(|| format!("Falta el atributo {key} en el libro.").into())
}

fn member(base: &str, target: &str) -> Result<String> {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        base.rsplit_once('/')
            .map(|(dir, _)| dir.split('/').collect())
            .unwrap_or_default()
    };
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or("Ruta interna de Excel no válida.")?;
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

struct Link {
    kind: String,
    path: String,
}

fn links(zip: &mut ZipArchive<File>, document: &str) -> Result<HashMap<String, Link>> {
    let (directory, name) = document.rsplit_once('/').unwrap_or(("", document));
    let path = format!(
        "{}{}_rels/{name}.rels",
        directory,
        if directory.is_empty() { "" } else { "/" }
    );
    let mut xml = reader(zip.by_name(&path)?);
    let mut buffer = Vec::new();
    let mut links = HashMap::new();
    loop {
        match xml.read_event_into(&mut buffer)? {
            Event::Start(tag) if tag.local_name().as_ref() == "Relationship" => {
                if attribute(&tag, "TargetMode")?.as_deref() != Some("External") {
                    let uri = required(&tag, "Type")?;
                    let kind = uri.rsplit('/').next().unwrap_or("").to_owned();
                    if matches!(
                        kind.as_str(),
                        "officeDocument" | "worksheet" | "styles" | "theme" | "sharedStrings"
                    ) {
                        let path = member(document, &required(&tag, "Target")?)?;
                        links.insert(required(&tag, "Id")?, Link { kind, path });
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(links)
}

struct Book {
    sheets: Vec<(String, String)>,
    strings: Vec<String>,
    styles: Option<Vec<u8>>,
    theme: Option<Vec<u8>>,
    date1904: bool,
    strict: bool,
}

impl Book {
    fn read(zip: &mut ZipArchive<File>, cancel: &AtomicBool) -> Result<Self> {
        let root = links(zip, "")?
            .into_values()
            .find(|l| l.kind == "officeDocument")
            .ok_or("El archivo no contiene un libro de Excel.")?
            .path;
        let targets = links(zip, &root)?;
        let mut sheets = Vec::new();
        let mut date1904 = false;
        let mut strict = false;
        {
            let mut xml = reader(zip.by_name(&root)?);
            let mut buffer = Vec::new();
            loop {
                match xml.read_event_into(&mut buffer)? {
                    Event::Start(tag) => match tag.local_name().as_ref() {
                        "workbook" => {
                            strict = tag
                                .attributes()
                                .flatten()
                                .any(|a| a.value == "http://purl.oclc.org/ooxml/spreadsheetml/main")
                        }
                        "workbookPr" => {
                            date1904 = matches!(
                                attribute(&tag, "date1904")?.as_deref(),
                                Some("1" | "true")
                            )
                        }
                        "sheet"
                            if !matches!(
                                attribute(&tag, "state")?.as_deref(),
                                Some("hidden" | "veryHidden")
                            ) =>
                        {
                            let id = tag
                                .attributes()
                                .collect::<std::result::Result<Vec<_>, _>>()?
                                .into_iter()
                                .find(|a| a.key.local_name().as_ref() == "id")
                                .ok_or("Hoja sin relación interna.")?;
                            if let Some(link) = targets
                                .get(id.value.as_ref())
                                .filter(|l| l.kind == "worksheet")
                            {
                                sheets.push((required(&tag, "name")?, link.path.clone()));
                            }
                        }
                        _ => {}
                    },
                    Event::Eof => break,
                    _ => {}
                }
                buffer.clear();
            }
        }
        let find = |kind: &str| targets.values().find(|v| v.kind == kind);
        let strings = if let Some(l) = find("sharedStrings") {
            rows::shared_strings(zip.by_name(&l.path)?, cancel)?
        } else {
            Vec::new()
        };
        let mut read_part = |kind: &str| -> Result<Option<Vec<u8>>> {
            let Some(l) = find(kind) else { return Ok(None) };
            let mut bytes = Vec::new();
            zip.by_name(&l.path)?.read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        };
        Ok(Self {
            sheets,
            strings,
            styles: read_part("styles")?,
            theme: read_part("theme")?,
            date1904,
            strict,
        })
    }
}

pub fn split(
    input: &Path,
    dest: &Path,
    limit: u64,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> io::Result<Outcome> {
    let mut run = || -> Result<Outcome> {
        check_cancel(cancel)?;
        let mut zip = ZipArchive::new(open_input(input)?)
            .map_err(|_| "No se pudo leer este Excel. Si está cifrado, guarda una copia .xlsx sin contraseña.")?;
        let book = Book::read(&mut zip, cancel)?;
        let stem: String = input
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .chars()
            .take(48)
            .collect();
        let dir = tempfile::Builder::new()
            .rand_bytes(16)
            .prefix(&format!("{stem} - Excel-"))
            .tempdir_in(dest)?;
        let folder_name = dir.path().file_name().unwrap().to_string_lossy();
        let batch = &folder_name[folder_name.len() - 16..];
        let mut files = Vec::new();
        let total = book.sheets.len() as u64 * 1000;
        for (i, (sheet, path)) in book.sheets.iter().enumerate() {
            let source = zip.by_name(path)?;
            let size = source.size().max(1);
            let base = i as u64 * 1000;
            let mut rows = rows::scan(source, &book.strings, dir.path(), cancel, |offset| {
                progress(
                    base + (offset as u128 * 500 / size as u128).min(500) as u64,
                    total,
                )
            })
            .map_err(|e| format!("Hoja «{sheet}». {e}"))?;
            let n = rows.len();
            if n > 0 {
                let template = package::template(&book, &[sheet.as_str()], Some(batch))?;
                package::parts(
                    &template,
                    &mut rows,
                    sheet,
                    &stem,
                    i + 1,
                    dir.path(),
                    limit,
                    cancel,
                    |done| progress(base + 500 + done as u64 * 500 / n as u64, total),
                    &mut files,
                )
                .map_err(|e| format!("Hoja «{sheet}». {e}"))?;
            }
            progress(base + 1000, total);
        }
        let count = files.len();
        if count == 0 {
            return Err("No se encontraron filas con datos en las hojas visibles.".into());
        }
        // The part count is known only after the last compressed workbook has closed.
        for (i, path) in files.iter().enumerate() {
            check_cancel(cancel)?;
            let name = path.file_stem().unwrap().to_str().unwrap();
            let (prefix, _) = name.rsplit_once(" - ").unwrap();
            let new_name = format!("{prefix} - {:03} de {count:03}.xlsx", i + 1);
            std::fs::rename(path, path.with_file_name(new_name))?;
        }
        Ok(Outcome {
            path: dir.keep(),
            parts: count as u64,
        })
    };
    run().map_err(io::Error::other)
}
