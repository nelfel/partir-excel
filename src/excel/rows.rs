use super::{Result, attribute, check_cancel, escape, reader};
use quick_xml::events::{BytesStart, Event};
use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
};

// Text is kept as escaped XML so an embedded line break, entity or Excel _xNNNN_
// sequence survives the move from a shared string to an inline cell.
fn append_text(event: &Event, value: &mut String) -> Result<()> {
    match event {
        Event::Text(t) => value.push_str(&escape(t.xml10_content())),
        Event::CData(t) => value.push_str(&escape(t.xml10_content())),
        Event::GeneralRef(r) => {
            if quick_xml::escape::resolve_predefined_entity(r.as_ref()).is_none()
                && r.resolve_char_ref()?.is_none()
            {
                return Err("Entidad XML desconocida.".into());
            }
            value.push('&');
            value.push_str(r.as_ref());
            value.push(';');
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn shared_strings(source: impl Read, cancel: &AtomicBool) -> Result<Vec<String>> {
    let mut xml = reader(source);
    let (mut strings, mut buffer) = (Vec::new(), Vec::new());
    let (mut text, mut phonetic) = (false, false);
    let mut value = String::new();
    loop {
        let event = xml.read_event_into(&mut buffer)?;
        match &event {
            Event::Start(t) => match t.local_name().as_ref() {
                "si" => {
                    check_cancel(cancel)?;
                    value.clear();
                }
                "rPh" => phonetic = true,
                "t" => text = !phonetic,
                _ => {}
            },
            Event::End(t) => match t.local_name().as_ref() {
                "si" => strings.push(std::mem::take(&mut value)),
                "rPh" => phonetic = false,
                "t" => text = false,
                _ => {}
            },
            Event::Eof => break,
            _ if text => append_text(&event, &mut value)?,
            _ => {}
        }
        buffer.clear();
    }
    Ok(strings)
}

pub(super) struct Rows {
    pub file: File,
    pub offsets: Vec<u64>,
    pub columns: String,
}
impl Rows {
    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }
}

fn allowed_attrs(tag: &BytesStart, names: &[&str], out: &mut String) -> Result<()> {
    for attr in tag.attributes() {
        let a = attr?;
        if names.contains(&a.key.as_ref()) {
            out.push(' ');
            out.push_str(a.key.as_ref());
            out.push_str("=\"");
            out.push_str(&escape(quick_xml::escape::unescape(&a.value)?));
            out.push('"');
        }
    }
    Ok(())
}

fn column_index(reference: &str) -> Result<u32> {
    let letters = reference.bytes().take_while(u8::is_ascii_uppercase).count();
    if !(1..=3).contains(&letters)
        || reference.len() == letters
        || !reference[letters..].bytes().all(|b| b.is_ascii_digit())
    {
        return Err(format!("Referencia de celda no válida: {reference}.").into());
    }
    let n = reference
        .bytes()
        .take(letters)
        .fold(0, |acc, b| acc * 26 + u32::from(b - b'A' + 1));
    if n > 16_384 {
        return Err("La columna supera el límite de Excel.".into());
    }
    Ok(n)
}

fn column_letters(mut n: u32) -> String {
    let mut letters = [0u8; 3];
    let mut at = letters.len();
    while n > 0 {
        n -= 1;
        at -= 1;
        letters[at] = b'A' + (n % 26) as u8;
        n /= 26;
    }
    String::from_utf8(letters[at..].to_vec()).unwrap()
}

#[derive(Default)]
struct Cell {
    column: u32,
    kind: String,
    style: Option<u32>,
    formula: bool,
    saved: bool,
    value: String,
}

impl Cell {
    fn start(&mut self, tag: &BytesStart) -> Result<()> {
        self.column = if let Some(r) = attribute(tag, "r")? {
            column_index(&r)?
        } else {
            self.column + 1
        };
        if self.column > 16_384 {
            return Err("Demasiadas columnas en una fila.".into());
        }
        self.kind = attribute(tag, "t")?.unwrap_or_default();
        self.style = attribute(tag, "s")?.map(|v| v.parse()).transpose()?;
        self.formula = false;
        self.saved = false;
        self.value.clear();
        Ok(())
    }

    fn append(&self, row: &mut String, source_row: u32, strings: &[String]) -> Result<()> {
        let col = column_letters(self.column);
        if self.formula && (!self.saved || (self.value.is_empty() && self.kind != "str")) {
            return Err(format!("La fórmula de {col}{source_row} no tiene un resultado guardado. Abre el original en Excel, recalcula, guarda y vuelve a intentarlo.").into());
        }
        if !self.saved {
            return Ok(());
        }
        let value = match self.kind.as_str() {
            "s" => strings
                .get(self.value.trim().parse::<usize>()?)
                .ok_or("Índice de texto compartido no válido.")?
                .as_str(),
            _ => &self.value,
        };
        let text = matches!(self.kind.as_str(), "s" | "str" | "inlineStr");
        if !text && value.is_empty() {
            return Ok(());
        }
        row.push_str("<c r=\"");
        row.push_str(&col);
        row.push('\0');
        row.push('"');
        if let Some(s) = self.style {
            row.push_str(&format!(" s=\"{s}\""));
        }
        if text {
            row.push_str(" t=\"inlineStr\"><is><t xml:space=\"preserve\">");
            row.push_str(value);
            row.push_str("</t></is></c>");
        } else {
            match self.kind.as_str() {
                "" | "n" => {}
                "b" | "e" | "d" => {
                    row.push_str(" t=\"");
                    row.push_str(&self.kind);
                    row.push('"');
                }
                unknown => return Err(format!("Tipo de celda no compatible: {unknown}.").into()),
            }
            row.push_str("><v>");
            row.push_str(value);
            row.push_str("</v></c>");
        }
        Ok(())
    }
}

pub(super) fn scan(
    source: impl Read,
    strings: &[String],
    dir: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<Rows> {
    let mut xml = reader(source);
    let mut writer = BufWriter::with_capacity(256 * 1024, tempfile::tempfile_in(dir)?);
    let (mut offsets, mut columns) = (vec![0u64], String::new());
    let (mut row, mut row_attrs, mut buffer) = (String::new(), String::new(), Vec::new());
    let (mut sheet_data, mut in_row, mut in_cell, mut text, mut phonetic) =
        (false, false, false, false, false);
    let mut cell = Cell::default();
    let (mut row_index, mut next_report) = (0u32, 0u64);
    loop {
        let event = xml.read_event_into(&mut buffer)?;
        match &event {
            Event::Start(t) => match t.local_name().as_ref() {
                "col" if !sheet_data => {
                    columns.push_str("<col");
                    allowed_attrs(
                        t,
                        &[
                            "min",
                            "max",
                            "width",
                            "style",
                            "hidden",
                            "bestFit",
                            "customWidth",
                            "outlineLevel",
                            "collapsed",
                        ],
                        &mut columns,
                    )?;
                    columns.push_str("/>");
                }
                "sheetData" => sheet_data = true,
                "row" if sheet_data => {
                    check_cancel(cancel)?;
                    in_row = true;
                    row.clear();
                    row_attrs.clear();
                    cell.column = 0;
                    row_index = attribute(t, "r")?
                        .map(|r| r.parse())
                        .transpose()?
                        .unwrap_or(row_index + 1);
                    if !(1..=1_048_576).contains(&row_index) {
                        return Err("Número de fila fuera del límite de Excel.".into());
                    }
                    allowed_attrs(
                        t,
                        &[
                            "s",
                            "customFormat",
                            "ht",
                            "hidden",
                            "customHeight",
                            "outlineLevel",
                            "collapsed",
                        ],
                        &mut row_attrs,
                    )?;
                }
                "c" if in_row => {
                    cell.start(t)?;
                    in_cell = true;
                }
                "f" if in_cell => cell.formula = true,
                "rPh" if in_cell => phonetic = true,
                "v" if in_cell => {
                    cell.saved = true;
                    text = true;
                }
                "is" if in_cell => cell.saved = true,
                "t" if in_cell => text = !phonetic,
                _ => {}
            },
            Event::End(t) => match t.local_name().as_ref() {
                "v" | "t" => text = false,
                "rPh" => phonetic = false,
                "c" if in_cell => {
                    cell.append(&mut row, row_index, strings)?;
                    in_cell = false;
                }
                "row" if in_row => {
                    if !row.is_empty() {
                        let prefix = format!("<row r=\"\0\"{row_attrs}>");
                        writer.write_all(prefix.as_bytes())?;
                        writer.write_all(row.as_bytes())?;
                        writer.write_all(b"</row>")?;
                        offsets
                            .push(offsets.last().unwrap() + (prefix.len() + row.len() + 6) as u64);
                    }
                    in_row = false;
                }
                "sheetData" => break,
                _ => {}
            },
            Event::Eof => return Err("Falta el cierre de los datos de la hoja.".into()),
            _ if text => append_text(&event, &mut cell.value)?,
            _ => {}
        }
        let offset = xml.buffer_position();
        if offset >= next_report {
            check_cancel(cancel)?;
            progress(offset);
            next_report = offset + 1024 * 1024;
        }
        buffer.clear();
    }
    Ok(Rows {
        file: writer.into_inner()?,
        offsets,
        columns,
    })
}
