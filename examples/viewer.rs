use pdfkit::{PdfDocument, PdfView};
use tontooui::elements::layout::View;
use tontooui::renderer::FontSystem;
use tontooui::renderer::window::{App, Key, Viewport, run};
use tontooui::theme::ThemeMode;
use vello::Scene;
use vello::peniko::Color;

/// Manual test viewer for PDFKit.
///
/// Usage: `cargo run --example viewer -- /path/to/file.pdf [password]`
/// Shows all pages stacked vertically; the mouse wheel scrolls,
/// arrow keys jump between pages.
struct Viewer {
  views: Vec<PdfView>,
  /// Unscrolled content offset of each page top (page 0 starts at
  /// 0), rebuilt every draw; arrow keys jump the scroll here.
  tops: Vec<f32>,
  /// Full content height (pages + gaps).
  total: f32,
  /// Current page for arrow-key jumps.
  current: usize,
  /// Vertical scroll offset in logical px.
  scroll: f32,
  /// Last seen viewport height (for clamping outside draw).
  view_h: f32,
  bg: Color,
}

impl Viewer {
  fn new(path: &str, password: &str) -> Result<Self, String> {
    let first = PdfDocument::load_file_with_password(path, password)
      .map_err(|e| format!("cannot open '{path}': {e}"))?;
    let count = first.page_count();
    let mut views = Vec::with_capacity(count);
    let mut first = Some(first);
    for n in 0..count {
      // PdfDocument is not Clone: reuse the parsed document for page
      // 0 and reload per remaining page, so every view keeps its own
      // image cache instead of re-uploading each frame.
      let doc = match first.take() {
        Some(doc) => doc,
        None => PdfDocument::load_file_with_password(path, password)
          .map_err(|e| format!("cannot open '{path}': {e}"))?,
      };
      let mut view = PdfView::new(doc);
      view.set_theme(ThemeMode::Dark);
      view.set_page(n);
      views.push(view);
    }
    Ok(Self { views, tops: Vec::new(), total: 0.0, current: 0, scroll: 0.0, view_h: 0.0, bg: Color::from_rgb8(27, 32, 34) })
  }

  fn clamp_scroll(&mut self) {
    let max = (self.total - self.view_h).max(0.0);
    self.scroll = self.scroll.clamp(0.0, max);
  }

  fn goto_page(&mut self, index: usize) {
    let index = index.min(self.views.len().saturating_sub(1));
    self.current = index;
    if let Some(top) = self.tops.get(index) {
      self.scroll = *top;
    }
    self.clamp_scroll();
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
    const GAP: f32 = 24.0;
    self.view_h = viewport.height;
    self.tops.clear();
    let mut y = viewport.y + 16.0 - self.scroll;
    for view in &mut self.views {
      let (w, h) = view.measure(fonts);
      let x = viewport.x + ((viewport.width - w) / 2.0).max(0.0);
      self.tops.push(y + self.scroll - viewport.y - 16.0);
      view.place(fonts, x, y, w, h);
      view.draw(scene, fonts, images);
      y += h + GAP;
    }
    self.total = y - GAP - viewport.y - 16.0 + self.scroll;
    self.clamp_scroll();
    // Keep arrow jumps coherent after manual wheel scrolling.
    for (i, top) in self.tops.iter().enumerate() {
      if *top <= self.scroll + 1.0 {
        self.current = i;
      }
    }
  }

  fn key(&mut self, key: Key) {
    match key {
      Key::Right | Key::Down => self.goto_page(self.current + 1),
      Key::Left | Key::Up => self.goto_page(self.current.saturating_sub(1)),
      _ => {}
    }
  }

  fn mouse_wheel(&mut self, _dx: f64, dy: f64) {
    // Wheel scrolls vertically (positive dy = wheel up = earlier content).
    self.scroll -= dy as f32 * 40.0;
    self.clamp_scroll();
  }

  fn background(&self) -> Color {
    self.bg
  }
}

fn main() {
  let path = std::env::args().nth(1).unwrap_or_default();
  let password = std::env::args().nth(2).unwrap_or_default();
  if path.is_empty() {
    eprintln!("usage: viewer <file.pdf> [password]");
    std::process::exit(2);
  }
  match Viewer::new(&path, &password) {
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
