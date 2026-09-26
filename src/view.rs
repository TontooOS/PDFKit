use std::any::Any;

use parley::Layout;
use tontooui::elements::layout::View;
use tontooui::renderer::images::ImageLoader;
use tontooui::renderer::text::{FontSystem, SolidBrush, draw_layout};
use tontooui::theme::ThemeMode;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Brush, Color, Fill};

use crate::document::PdfDocument;
use crate::page::PdfTextRun;

/// Page background in dark mode (TontooOS default app background).
pub const PDF_BG_DARK: Color = Color::from_rgb8(27, 32, 34);
/// Page background in light mode.
pub const PDF_BG_LIGHT: Color = Color::from_rgb8(255, 255, 255);
/// Default PDF text color in dark mode.
pub const PDF_TEXT_DARK: Color = Color::from_rgb8(216, 217, 217);
/// Default PDF text color in light mode.
pub const PDF_TEXT_LIGHT: Color = Color::from_rgb8(39, 39, 39);

/// A real page-based PDF view for TontooUI.
///
/// The view owns a `PdfDocument` and renders the current page as
/// positioned text runs through the shared `FontSystem` (SF Pro via
/// system fonts, Parley layout, Vello glyphs). Nothing is rasterized
/// to a bitmap: every `PdfTextRun` keeps its PDF coordinates, so the
/// later editor milestone can select and edit text in place.
///
/// v0.1 renders text only and maps all runs to the theme text color
/// (PDF color operators, images, paths and annotations follow in
/// later milestones).
pub struct PdfView {
  doc: PdfDocument,
  page_no: usize,
  zoom: f32,
  dark: bool,
  x: f32,
  y: f32,
  width: f32,
  height: f32,
  layouts: Vec<Layout<SolidBrush>>,
  layout_scale: f32,
  dirty: bool,
}

impl PdfView {
  /// Create a view over `doc`, showing page 0 at 1x zoom.
  pub fn new(doc: PdfDocument) -> Self {
    Self {
      doc,
      page_no: 0,
      zoom: 1.0,
      dark: true,
      x: 0.0,
      y: 0.0,
      width: 0.0,
      height: 0.0,
      layouts: Vec::new(),
      layout_scale: 0.0,
      dirty: true,
    }
  }

  /// Number of pages in the document.
  pub fn page_count(&self) -> usize {
    self.doc.page_count()
  }

  /// Currently shown page (zero-based).
  pub fn current_page(&self) -> usize {
    self.page_no
  }

  /// Show page `index`. Out-of-range indices are clamped.
  pub fn set_page(&mut self, index: usize) {
    let clamped = index.min(self.doc.page_count().saturating_sub(1));
    if clamped != self.page_no {
      self.page_no = clamped;
      self.dirty = true;
    }
  }

  /// Advance one page, staying on the last page.
  pub fn next_page(&mut self) {
    self.set_page(self.page_no + 1);
  }

  /// Go back one page, staying on the first page.
  pub fn prev_page(&mut self) {
    self.set_page(self.page_no.saturating_sub(1));
  }

  /// Set the zoom factor (clamped to `0.25..=8.0`).
  pub fn set_zoom(&mut self, zoom: f32) {
    let clamped = zoom.clamp(0.25, 8.0);
    if (clamped - self.zoom).abs() > f32::EPSILON {
      self.zoom = clamped;
      self.dirty = true;
    }
  }

  /// Current zoom factor.
  pub fn zoom(&self) -> f32 {
    self.zoom
  }

  /// Follow the system theme (dark page background vs paper white).
  pub fn set_theme(&mut self, mode: ThemeMode) {
    let dark = mode == ThemeMode::Dark;
    if dark != self.dark {
      self.dark = dark;
      self.dirty = true;
    }
  }

  /// Placed rect (x, y, width, height) in logical px.
  pub fn rect(&self) -> (f32, f32, f32, f32) {
    (self.x, self.y, self.width, self.height)
  }

  fn page_size(&self) -> (f32, f32) {
    match self.doc.page(self.page_no) {
      Ok(page) => (page.width * self.zoom, page.height * self.zoom),
      Err(_) => (0.0, 0.0),
    }
  }

  fn text_color(&self) -> Color {
    if self.dark { PDF_TEXT_DARK } else { PDF_TEXT_LIGHT }
  }

  fn bg_color(&self) -> Color {
    if self.dark { PDF_BG_DARK } else { PDF_BG_LIGHT }
  }

  fn runs(&self) -> &[PdfTextRun] {
    match self.doc.page(self.page_no) {
      Ok(page) => &page.runs,
      Err(_) => &[],
    }
  }

  fn ensure_layouts(&mut self, fonts: &mut FontSystem) {
    if !self.dirty && !self.layouts.is_empty() && self.layout_scale == fonts.scale {
      return;
    }
    self.layouts.clear();
    let color = self.text_color();
    // Clone runs to satisfy the borrow checker (doc vs fonts borrows).
    let runs: Vec<PdfTextRun> = self.runs().to_vec();
    for run in &runs {
      let weight = if run.bold { 700.0 } else { 400.0 };
      let layout = fonts.layout_text_weighted(&run.text, run.font_size * self.zoom, color, weight, None);
      self.layouts.push(layout);
    }
    self.layout_scale = fonts.scale;
    self.dirty = false;
  }

  /// View position of a run: PDF origin is bottom-left in points,
  /// the view origin is top-left in logical px.
  fn run_origin(&self, run: &PdfTextRun) -> (f32, f32) {
    let page_h = self.doc.page(self.page_no).map(|p| p.height).unwrap_or(0.0);
    let ox = self.x + run.x * self.zoom;
    // Baseline-to-top approximation (ascent ~= font size); exact
    // metrics need embedded fonts (later milestone).
    let oy = self.y + (page_h - run.y) * self.zoom - run.font_size * self.zoom;
    (ox, oy)
  }

  fn render(&mut self, scene: &mut Scene, fonts: &mut FontSystem) {
    let scale = fonts.scale;
    self.ensure_layouts(fonts);
    let bg = self.bg_color();
    let (page_w, page_h) = self.page_size();
    scene.fill(
      Fill::NonZero,
      Affine::translate((self.x as f64, self.y as f64)),
      &Brush::Solid(bg),
      None,
      &Rect::new(0.0, 0.0, page_w as f64, page_h as f64),
    );
    let runs: Vec<PdfTextRun> = self.runs().to_vec();
    for (run, layout) in runs.iter().zip(self.layouts.iter()) {
      let (ox, oy) = self.run_origin(run);
      draw_layout(scene, layout, ox, oy, scale);
    }
  }
}

impl View for PdfView {
  fn measure(&mut self, fonts: &mut FontSystem) -> (f32, f32) {
    self.ensure_layouts(fonts);
    self.page_size()
  }

  fn place(&mut self, fonts: &mut FontSystem, x: f32, y: f32, w: f32, h: f32) {
    let _ = (w, h);
    self.ensure_layouts(fonts);
    let (page_w, page_h) = self.page_size();
    self.x = x;
    self.y = y;
    self.width = page_w;
    self.height = page_h;
  }

  fn draw(&mut self, scene: &mut Scene, fonts: &mut FontSystem, _images: &mut ImageLoader<'_>) {
    self.render(scene, fonts);
  }

  fn as_any_mut(&mut self) -> &mut dyn Any {
    self
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parser::tests::minimal_pdf;

  fn doc() -> PdfDocument {
    let pdf = minimal_pdf(b"BT /F1 12 Tf 72 720 Td (Hello View) Tj ET");
    PdfDocument::load_bytes(pdf).unwrap()
  }

  #[test]
  fn measures_letter_page() {
    let mut fonts = FontSystem::new();
    let mut view = PdfView::new(doc());
    let (w, h) = view.measure(&mut fonts);
    assert_eq!((w, h), (612.0, 792.0));
  }

  #[test]
  fn zoom_scales_measure() {
    let mut fonts = FontSystem::new();
    let mut view = PdfView::new(doc());
    view.set_zoom(2.0);
    let (w, h) = view.measure(&mut fonts);
    assert_eq!((w, h), (1224.0, 1584.0));
  }

  #[test]
  fn page_nav_clamps() {
    let mut view = PdfView::new(doc());
    view.set_page(99);
    assert_eq!(view.current_page(), 0);
    view.next_page();
    assert_eq!(view.current_page(), 0);
    view.prev_page();
    assert_eq!(view.current_page(), 0);
  }
}
