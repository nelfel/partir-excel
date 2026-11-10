use super::{Book, MAIN, OFFICE, REL, Result, TYPES, check_cancel, escape, rows::Rows};
use std::{
    fs::File,
    io::{BufReader, Cursor, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::AtomicBool,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

fn options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(5))
}

pub(super) struct Template {
    zip: Vec<u8>,
    namespace: &'static str,
}

pub(super) fn template(book: &Book, sheet: &str) -> Result<Template> {
    let (namespace, office) = if book.strict {
        (
            "http://purl.oclc.org/ooxml/spreadsheetml/main",
            "http://purl.oclc.org/ooxml/officeDocument/relationships",
        )
    } else {
        (MAIN, OFFICE)
    };
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let mut add = |name: &str, data: &[u8]| -> Result<()> {
        archive.start_file(name, options())?;
        archive.write_all(data)?;
        Ok(())
    };
    let mut types = format!(
        r#"<Types xmlns="{TYPES}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#
    );
    let mut rels = format!(
        r#"<Relationships xmlns="{REL}"><Relationship Id="rId1" Type="{office}/worksheet" Target="worksheets/sheet1.xml"/>"#
    );
    if let Some(styles) = &book.styles {
        add("xl/styles.xml", styles)?;
        types.push_str(r#"<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>"#);
        rels.push_str(&format!(
            r#"<Relationship Id="rId2" Type="{office}/styles" Target="styles.xml"/>"#
        ));
    }
    if let Some(theme) = &book.theme {
        add("xl/theme/theme1.xml", theme)?;
        types.push_str(r#"<Override PartName="/xl/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>"#);
        rels.push_str(&format!(
            r#"<Relationship Id="rId3" Type="{office}/theme" Target="theme/theme1.xml"/>"#
        ));
    }
    types.push_str("</Types>");
    rels.push_str("</Relationships>");
    add("[Content_Types].xml", types.as_bytes())?;
    add("xl/_rels/workbook.xml.rels", rels.as_bytes())?;
    add("_rels/.rels", format!(r#"<Relationships xmlns="{REL}"><Relationship Id="rId1" Type="{office}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#).as_bytes())?;
    add("xl/workbook.xml", format!(r#"<workbook xmlns="{namespace}" xmlns:r="{office}"><workbookPr date1904="{}"/><sheets><sheet name="{}" sheetId="1" r:id="rId1"/></sheets></workbook>"#, u8::from(book.date1904), escape(sheet)).as_bytes())?;
    Ok(Template {
        zip: archive.finish()?.into_inner(),
        namespace,
    })
}

struct RowBuffer {
    raw: Vec<u8>,
    rendered: Vec<u8>,
}

impl RowBuffer {
    fn write(
        &mut self,
        source: &mut impl Read,
        output: &mut impl Write,
        bytes: u64,
        number: usize,
    ) -> Result<()> {
        self.raw.resize(usize::try_from(bytes)?, 0);
        source.read_exact(&mut self.raw)?;
        self.rendered.clear();
        let row_number = number.to_string();
        let mut pieces = self.raw.split(|c| *c == 0);
        self.rendered.extend_from_slice(pieces.next().unwrap());
        for piece in pieces {
            self.rendered.extend_from_slice(row_number.as_bytes());
            self.rendered.extend_from_slice(piece);
        }
        output.write_all(&self.rendered)?;
        Ok(())
    }
}

fn write_candidate(
    template: &Template,
    rows: &mut Rows,
    start: usize,
    end: usize,
    path: &Path,
    limit: u64,
    cancel: &AtomicBool,
) -> Result<(usize, u64)> {
    let file = File::create_new(path)?;
    let observer = file.try_clone()?;
    let mut out = ZipWriter::new(file);
    // Desktop Excel rejects an old ZIP directory between the last two parts.
    // Copy compressed entries into one fresh archive with a single directory.
    let mut common = zip::ZipArchive::new(Cursor::new(&template.zip))?;
    for i in 0..common.len() {
        out.raw_copy_file(common.by_index(i)?)?;
    }
    out.start_file("xl/worksheets/sheet1.xml", options())?;
    write!(out, r#"<worksheet xmlns="{}">"#, template.namespace)?;
    if !rows.columns.is_empty() {
        write!(out, "<cols>{}</cols>", rows.columns)?;
    }
    out.write_all(b"<sheetData>")?;
    let mut buf = RowBuffer {
        raw: Vec::new(),
        rendered: Vec::new(),
    };
    let mut source = BufReader::with_capacity(256 * 1024, &mut rows.file);
    source.seek(SeekFrom::Start(0))?;
    buf.write(&mut source, &mut out, rows.offsets[1], 1)?;
    source.seek(SeekFrom::Start(rows.offsets[start]))?;
    let mut next = start;
    let mut check_at = rows.offsets[start];
    while next < end {
        check_cancel(cancel)?;
        buf.write(
            &mut source,
            &mut out,
            rows.offsets[next + 1] - rows.offsets[next],
            next - start + 2,
        )?;
        next += 1;
        if rows.offsets[next] >= check_at {
            if observer.metadata()?.len() >= limit - (limit / 40).min(64 * 1024) {
                break;
            }
            check_at = rows.offsets[next] + 64 * 1024;
        }
    }
    out.write_all(b"</sheetData></worksheet>")?;
    let file = out.finish()?;
    Ok((next, file.metadata()?.len()))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn parts(
    template: &Template,
    rows: &mut Rows,
    sheet: &str,
    stem: &str,
    sheet_index: usize,
    dir: &Path,
    limit: u64,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize),
) -> Result<u64> {
    let sheet_name: String = sheet
        .chars()
        .map(|c| {
            if c.is_control() || r#"\/:"*?<>|"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut start = 1;
    let mut part = 1;
    loop {
        let path = dir.join(format!(
            "{stem} - {sheet_index:02} {sheet_name} - {part:03}.xlsx"
        ));
        let mut end = rows.len();
        loop {
            check_cancel(cancel)?;
            let (next, actual_size) =
                write_candidate(template, rows, start, end, &path, limit, cancel)?;
            if actual_size <= limit {
                start = next;
                progress(next);
                break;
            }
            // Deflate can retain a block until the ZIP is closed. Only the finished
            // file is authoritative. Retry the same rows with a smaller range.
            std::fs::remove_file(&path)?;
            let count = next - start;
            if count <= 1 {
                return Err(format!("El encabezado y una fila, o la estructura de la hoja, exceden el máximo de {limit} bytes. Aumenta el tamaño por parte.").into());
            }
            let estimate = (count as u128 * u128::from(limit) / u128::from(actual_size)) as usize;
            end = start + estimate.clamp(1, count - 1);
        }
        if start >= rows.len() {
            return Ok(part);
        }
        part += 1;
    }
}
