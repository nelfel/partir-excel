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
