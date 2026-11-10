use super::*;
use eframe::App as _;
use egui::{DroppedFile, Event, FullOutput, PointerButton, Pos2, RawInput, Rect, Shape};
use std::time::Instant;

fn draw(app: &mut App, ctx: &egui::Context, file: Option<&Path>, events: Vec<Event>) -> FullOutput {
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(640., 610.))),
        dropped_files: file
            .into_iter()
            .map(|p| DroppedFile {
                path: Some(p.to_owned()),
                ..Default::default()
            })
            .collect(),
        events,
        ..Default::default()
    };
    ctx.run(input, |ctx| {
        app.update(ctx, &mut eframe::Frame::_new_kittest())
    })
}

fn click(app: &mut App, ctx: &egui::Context, output: FullOutput, label: &str) {
    let pos = output
        .shapes
        .iter()
        .find_map(|item| match &item.shape {
            Shape::Text(t) if t.galley.text() == label => Some(t.pos + t.galley.size() * 0.5),
            _ => None,
        })
        .unwrap_or_else(|| panic!("Falta el botón {label}"));
    for pressed in [true, false] {
        draw(
            app,
            ctx,
            None,
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ],
        );
    }
}

fn wait_for_output(app: &mut App, ctx: &egui::Context) -> PathBuf {
    let until = Instant::now() + Duration::from_secs(30);
    while app.job.is_some() && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
        draw(app, ctx, None, vec![]);
    }
    app.result
        .as_ref()
        .unwrap_or_else(|| panic!("Falta el resultado. {:?}", app.message))
        .path
        .clone()
}

#[test]
fn drop_split_and_reconstruct_through_the_buttons() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let book = tmp.path().join("Mi informe.xlsx");
    if let Some(fixture) = std::env::var_os("PARTIR_EXCEL_TEST_BOOK") {
        fs::copy(fixture, &book)?;
    } else {
        fs::write(&book, vec![19; 6721])?;
    }
    let input = fs::read(&book)?;
    let mut app = App {
        mode: Mode::Split,
        size: "0,001".into(),
        ..Default::default()
    };
    let ctx = egui::Context::default();
    setup(&ctx);
    draw(&mut app, &ctx, Some(&book), vec![]);
    let out = draw(&mut app, &ctx, None, vec![]);
    click(&mut app, &ctx, out, "Dividir archivo");
    let folder = wait_for_output(&mut app, &ctx);
    let index = folder.join(INDEX);
    assert_eq!(
        transfer::read_index(&index)?.partes.len() as u64,
        part_count(input.len() as u64, 1000)?
    );
    draw(&mut app, &ctx, Some(&index), vec![]);
    let out = draw(&mut app, &ctx, None, vec![]);
    click(&mut app, &ctx, out, "Reconstruir Excel");
    let rebuilt = wait_for_output(&mut app, &ctx);
    assert_eq!(fs::read(rebuilt)?, input);
    Ok(())
}

#[test]
fn independent_books_from_the_default_button() -> Result<(), Box<dyn std::error::Error>> {
    use calamine::{Data, Reader, Xlsx, open_workbook};
    use sha2::{Digest, Sha256};
    use std::io::Write;
    use zip::{ZipWriter, write::SimpleFileOptions};

    let tmp = tempfile::tempdir()?;
    let input = tmp.path().join("Informe.xlsx");
    let mut zip = ZipWriter::new(fs::File::create(&input)?);
    let mut add = |name: &str, xml: &str| -> io::Result<()> {
        zip.start_file(name, SimpleFileOptions::default())?;
        zip.write_all(xml.as_bytes())
    };
    let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let rel = "http://schemas.openxmlformats.org/package/2006/relationships";
    let office = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    add(
        "_rels/.rels",
        &format!(
            r#"<Relationships xmlns="{rel}"><Relationship Id="book" Target="xl/workbook.xml" Type="{office}/officeDocument"/></Relationships>"#
        ),
    )?;
    add(
        "xl/workbook.xml",
        &format!(
            r#"<workbook xmlns="{ns}" xmlns:r="{office}"><workbookPr date1904="1"/><sheets><sheet name="Ventas &amp; Sur" sheetId="1" r:id="a"/><sheet name="Totales" sheetId="2" r:id="b"/><sheet name="Interno" sheetId="3" r:id="c" state="hidden"/></sheets></workbook>"#
        ),
    )?;
    let mut links = String::new();
    for (id, path, kind) in [
        ("a", "worksheets/a.xml", "worksheet"),
        ("b", "worksheets/b.xml", "worksheet"),
        ("c", "worksheets/c.xml", "worksheet"),
        ("s", "sharedStrings.xml", "sharedStrings"),
        ("f", "styles.xml", "styles"),
    ] {
        links.push_str(&format!(
            r#"<Relationship Id="{id}" Target="{path}" Type="{office}/{kind}"/>"#
        ));
    }
    add(
        "xl/_rels/workbook.xml.rels",
        &format!(r#"<Relationships xmlns="{rel}">{links}</Relationships>"#),
    )?;
    add(
        "xl/styles.xml",
        &format!(
            r#"<styleSheet xmlns="{ns}"><fonts count="1"><font><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf/></cellStyleXfs><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs></styleSheet>"#
        ),
    )?;
    add(
        "xl/sharedStrings.xml",
        &format!(
            r#"<sst xmlns="{ns}"><si><r><t>Código </t></r><r><t>&amp; región</t></r><rPh sb="0" eb="1"><t>ignorar</t></rPh></si></sst>"#
        ),
    )?;
    let mut body = format!(
        r#"<worksheet xmlns="{ns}"><cols><col min="1" max="1" width="25" customWidth="1"/></cols><sheetData><row r="3"><c r="A3" t="s"><v>0</v></c></row>"#
    );
    for n in 1..=500u32 {
        let row = n + 3;
        let text = format!("{:x}", Sha256::digest(n.to_le_bytes()));
        body.push_str(&format!(r#"<row r="{row}"><c r="A{row}" t="inlineStr"><is><t>{text}</t></is></c><c r="C{row}" s="1"><v>44321</v></c><c r="F{row}"><f>1+{n}-1</f><v>{n}</v></c></row>"#));
    }
    body.push_str("</sheetData></worksheet>");
    add("xl/worksheets/a.xml", &body)?;
    let one_cell = format!(
        r#"<worksheet xmlns="{ns}"><sheetData><row><c t="inlineStr"><is><t>Total</t></is></c></row></sheetData></worksheet>"#
    );
    add("xl/worksheets/b.xml", &one_cell)?;
    add("xl/worksheets/c.xml", &one_cell)?;
    zip.finish()?;

    let ctx = egui::Context::default();
    setup(&ctx);
    let mut app = App {
        size: "0,006".into(),
        ..Default::default()
    };
    draw(&mut app, &ctx, Some(&input), vec![]);
    let output = draw(&mut app, &ctx, None, vec![]);
    click(&mut app, &ctx, output, "Crear archivos .xlsx");
    let folder = wait_for_output(&mut app, &ctx);
    let mut paths = fs::read_dir(folder)?
        .map(|item| item.map(|i| i.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();
    assert!(paths.len() > 2);
    let mut index = 0u32;
    let mut total_sheets = 0;
    for path in &paths {
        assert_eq!(path.extension().unwrap(), "xlsx");
        assert!(fs::metadata(&path)?.len() <= 6000);
        let mut archive = zip::ZipArchive::new(fs::File::open(&path)?)?;
        let mut next_header = 0;
        for i in 0..archive.len() {
            let entry = archive.by_index(i)?;
            assert_eq!(
                entry.header_start(),
                next_header,
                "Registro ZIP intermedio incompatible con Excel de escritorio"
            );
            next_header = entry.data_start().unwrap() + entry.compressed_size();
        }
        assert_eq!(next_header, archive.central_directory_start());
        let mut part: Xlsx<_> = open_workbook(&path)?;
        assert!(part.has_1904_epoch());
        let names = part.sheet_names();
        assert_eq!(names.len(), 1);
        let data = part.worksheet_range(&names[0])?;
        if names[0] == "Totales" {
            assert_eq!(data.get((0, 0)), Some(&Data::String("Total".into())));
            total_sheets += 1;
            continue;
        }
        assert_eq!(names[0], "Ventas & Sur");
        assert_eq!(
            data.get((0, 0)),
            Some(&Data::String("Código & región".into()))
        );
        assert!(part.worksheet_formula(&names[0])?.is_empty());
        for row in data.rows().skip(1) {
            index += 1;
            assert_eq!(
                row[0],
                Data::String(format!("{:x}", Sha256::digest(index.to_le_bytes())))
            );
            assert_eq!(row[1], Data::Empty);
            assert!(matches!(row[2], Data::DateTime(date) if date.as_f64() == 44321.));
            assert_eq!(row[5], Data::Float(index as f64));
        }
    }
    assert_eq!((index, total_sheets), (500, 1));
    let dest = tempfile::tempdir()?;
    let cancel = AtomicBool::new(false);
    assert!(excel::join(&paths[..paths.len() - 1], dest.path(), &cancel, |_, _| {}).is_err());
    let mut repeated = paths.clone();
    repeated[1] = repeated[0].clone();
    assert!(excel::join(&repeated, dest.path(), &cancel, |_, _| {}).is_err());
    assert!(excel::join(&paths, dest.path(), &AtomicBool::new(true), |_, _| {}).is_err());
    let other = excel::split(&input, tmp.path(), 6000, &cancel, |_, _| {})?;
    let mut mixed = paths.clone();
    mixed[0] = other.path.join(paths[0].file_name().unwrap());
    assert!(excel::join(&mixed, dest.path(), &cancel, |_, _| {}).is_err());
    assert_eq!(fs::read_dir(dest.path())?.count(), 0);

    let output = draw(&mut app, &ctx, None, vec![]);
    click(&mut app, &ctx, output, "Unir Excel");
    assert!(app.mode == Mode::JoinExcel);
    paths.reverse();
    app.select_parts(paths.clone());
    app.dest = Some(dest.path().to_owned());
    let output = draw(&mut app, &ctx, None, vec![]);
    click(&mut app, &ctx, output, "Unir en un .xlsx");
    let joined = wait_for_output(&mut app, &ctx);
    assert_eq!(joined.file_name().unwrap(), "Informe - unido.xlsx");
    let mut result: Xlsx<_> = open_workbook(&joined)?;
    assert!(result.has_1904_epoch());
    assert_eq!(result.sheet_names(), ["Ventas & Sur", "Totales"]);
    assert_eq!(
        result.worksheet_range("Totales")?.get((0, 0)),
        Some(&Data::String("Total".into()))
    );
    let data = result.worksheet_range("Ventas & Sur")?;
    assert_eq!(data.height(), 501);
    assert_eq!(
        data.get((0, 0)),
        Some(&Data::String("Código & región".into()))
    );
    for (i, row) in data.rows().skip(1).enumerate() {
        let n = i as u32 + 1;
        assert_eq!(
            row[0],
            Data::String(format!("{:x}", Sha256::digest(n.to_le_bytes())))
        );
        assert!(matches!(row[2],Data::DateTime(d) if d.as_f64()==44321.));
        assert_eq!(row[5], Data::Float(n as f64));
    }
    let another = excel::join(&paths, dest.path(), &cancel, |_, _| {})?;
    assert_eq!(
        another.path.file_name().unwrap(),
        "Informe - unido (2).xlsx"
    );
    let dest = tempfile::tempdir()?;
    assert!(excel::split(&input, dest.path(), 64, &AtomicBool::new(false), |_, _| {}).is_err());
    assert_eq!(fs::read_dir(dest.path())?.count(), 0);
    Ok(())
}

#[test]
#[ignore = "Prueba manual con un libro de gran tamaño y una carpeta de salida"]
fn large_workbook_from_disk() -> io::Result<()> {
    let input = PathBuf::from(std::env::var_os("PARTIR_EXCEL_QA_BOOK").expect("Excel de prueba"));
    let dest = PathBuf::from(std::env::var_os("PARTIR_EXCEL_QA_DIR").expect("Carpeta de prueba"));
    let output = excel::split(
        &input,
        &dest,
        15_000_000,
        &AtomicBool::new(false),
        |_, _| {},
    )?;
    println!("{} partes en {}", output.parts, output.path.display());
    Ok(())
}

#[test]
#[ignore = "Prueba manual con partes creadas por una versión anterior"]
fn join_books_from_disk() -> io::Result<()> {
    let folder =
        PathBuf::from(std::env::var_os("PARTIR_EXCEL_QA_PARTS").expect("Carpeta de las partes"));
    let dest = PathBuf::from(std::env::var_os("PARTIR_EXCEL_QA_DIR").expect("Carpeta de destino"));
    let mut paths = fs::read_dir(folder)?
        .map(|e| e.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.retain(|p| {
        p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))
    });
    paths.sort_by(|a, b| b.cmp(a));
    let output = excel::join(&paths, &dest, &AtomicBool::new(false), |_, _| {})?;
    println!(
        "{} partes reunidas en {}",
        output.parts,
        output.path.display()
    );
    Ok(())
}
