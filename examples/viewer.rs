use pdfkit::{PdfDocument, PdfView};
use tontooui::elements::layout::View;
use tontooui::renderer::FontSystem;
use tontooui::renderer::window::{App, Key, Viewport, run};
use tontooui::theme::ThemeMode;
use vello::Scene;
use vello::peniko::Color;

/// Manual test viewer for PDFKit.
///
/// Usage: `cargo run --example viewer -- /path/to/file.pdf`
/// Arrow keys turn pages, the mouse wheel zooms.
struct Viewer {
  view: PdfView,
  bg: Color,
}

impl Viewer {
  fn new(path: &str) -> Result<Self, String> {
    let doc = PdfDocument::load_file(path).map_err(|e| format!("cannot open '{path}': {e}"))?;
    let mut view = PdfView::new(doc);
    view.set_theme(ThemeMode::Dark);
    Ok(Self { view, bg: Color::from_rgb8(27, 32, 34) })
  }
}

impl App for Viewer {
  fn draw(
    &mut self,
    scene: &mut Scene,
    fonts: &mut FontSystem,
    images: &mut tontooui::renderer::ImageLoader<'_>,
    viewport: Viewport,
    _time_secs: f64,
  ) {
    let (w, h) = self.view.measure(fonts);
    let x = viewport.x + ((viewport.width - w) / 2.0).max(0.0);
    self.view.place(fonts, x, viewport.y + 16.0, w, h);
    self.view.draw(scene, fonts, images);
  }

  fn key(&mut self, key: Key) {
    match key {
      Key::Right | Key::Down => self.view.next_page(),
      Key::Left | Key::Up => self.view.prev_page(),
      _ => {}
    }
  }

  fn mouse_wheel(&mut self, _dx: f64, dy: f64) {
    if dy > 0.0 {
      self.view.set_zoom(self.view.zoom() * 1.1);
    } else if dy < 0.0 {
      self.view.set_zoom(self.view.zoom() / 1.1);
    }
  }

  fn background(&self) -> Color {
    self.bg
  }
}

fn main() {
  let path = std::env::args().nth(1).unwrap_or_default();
  if path.is_empty() {
    eprintln!("usage: viewer <file.pdf>");
    std::process::exit(2);
  }
  match Viewer::new(&path) {
    Ok(app) => {
      if let Err(err) = run("PDF Viewer", 720, 900, app) {
        eprintln!("error: {err}");
        std::process::exit(1);
      }
    }
    Err(err) => {
      eprintln!("error: {err}");
      std::process::exit(1);
    }
  }
}
