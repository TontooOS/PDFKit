use std::any::Any;

use tontooui::elements::layout::View;
use tontooui::renderer::images::ImageLoader;
use tontooui::renderer::text::{CTLine, CrispOpts, FontSystem, draw_line};
use tontooui::theme::ThemeMode;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Join, Point, Rect, Stroke};
use vello::peniko::{Brush, Color, ColorStop, Extend, Fill, Gradient, ImageAlphaType, ImageData, ImageFormat};

use crate::document::PdfDocument;
use crate::graphics::{FillRule, GradientItem, PageItem, PathItem, PathSeg, PlacedImage, Rgb};
use crate::page::PdfTextRun;

/// Page paper color (real viewers keep paper white in both themes;
/// the window chrome around the page follows the theme instead).
pub const PDF_PAPER: Color = Color::from_rgb8(255, 255, 255);
/// Page background in dark mode (kept for API compatibility).
pub const PDF_BG_DARK: Color = Color::from_rgb8(27, 32, 34);
/// Page background in light mode (kept for API compatibility).
pub const PDF_BG_LIGHT: Color = Color::from_rgb8(255, 255, 255);
/// Default PDF text color in dark mode (kept for API compatibility).
pub const PDF_TEXT_DARK: Color = Color::from_rgb8(216, 217, 217);
/// Default PDF text color in light mode (kept for API compatibility).
pub const PDF_TEXT_LIGHT: Color = Color::from_rgb8(39, 39, 39);

/// One laid-out text run with its view position.
struct RunLayout {
  layout: CTLine,
  x: f32,
  y: f32,
}

/// A real page-based PDF view for TontooUI.
///
/// The view owns a `PdfDocument` and renders the current page from its
/// vector/text item model through the shared `FontSystem` (SF Pro via
/// system fonts, Parley layout, Vello glyphs) and Vello paths. Nothing
/// is rasterized to a bitmap: every run and path keeps its PDF
/// coordinates, so the editor milestone can select and edit in place.
pub struct PdfView {
  doc: PdfDocument,
  page_no: usize,
  zoom: f32,
  dark: bool,
  x: f32,
  y: f32,
  width: f32,
  height: f32,
  runs: Vec<RunLayout>,
  layout_scale: f32,
  dirty: bool,
  img_page: usize,
  images: std::collections::HashMap<(usize, usize), ImageData>,
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
      runs: Vec::new(),
      layout_scale: 0.0,
      dirty: true,
      img_page: usize::MAX,
      images: std::collections::HashMap::new(),
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

  /// Follow the system theme (window chrome; the paper stays white
  /// like in standard viewers while PDF colors are honored).
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

  /// Map a user-space point to view logical px (y flipped).
  fn map_point(&self, ox: f32, oy1: f32, ux: f32, uy: f32) -> (f32, f32) {
    (self.x + (ux - ox) * self.zoom, self.y + (oy1 - uy) * self.zoom)
  }

  /// View position of a run: the CoreText pipeline draws in device
  /// px (`fonts.scale` per logical px), while paths and the paper use
  /// zoomed points. The layout is therefore built at
  /// `font_size * zoom / scale` so its device advances land back on
  /// zoomed points, and the draw origin is pre-divided by `scale`
  /// (the draw call re-multiplies). The vertical origin is the PDF
  /// baseline minus the measured first-line baseline of the laid-out
  /// line, not a `font_size` estimate.
  fn run_origin(
    &self,
    run: &PdfTextRun,
    ox: f32,
    oy1: f32,
    baseline_device: f32,
    scale: f32,
  ) -> (f32, f32) {
    let (px, py) = self.map_point(ox, oy1, run.x, run.y);
    (px / scale, (py - baseline_device) / scale)
  }

  fn ensure_layouts(&mut self, fonts: &mut FontSystem) {
    if !self.dirty && !self.runs.is_empty() && self.layout_scale == fonts.scale {
      return;
    }
    self.runs.clear();
    let page = match self.doc.page(self.page_no) {
      Ok(page) => page.clone(),
      Err(_) => {
        self.layout_scale = fonts.scale;
        self.dirty = false;
        return;
      }
    };
    let scale = fonts.scale.max(0.5);
    let (ox, oy1) = (page.origin_x, page.origin_y + page.height);
    for item in &page.items {
      if let PageItem::Text(run) = item {
        let weight = if run.bold { 700.0 } else { 400.0 };
        let layout = fonts.framesetter().create_line(
          &run.text,
          run.font_size * self.zoom / scale,
          rgba(run_rgb(run), run.alpha),
          weight,
          run.italic,
          0.0,
        );
        let baseline = line_baseline(&layout);
        let (x, y) = self.run_origin(run, ox, oy1, baseline, scale);
        self.runs.push(RunLayout { layout, x, y });
      }
    }
    self.layout_scale = fonts.scale;
    self.dirty = false;
  }

  fn path_shape(&self, item: &PathItem, ox: f32, oy1: f32) -> BezPath {
    let mut shape = BezPath::new();
    for sub in &item.subpaths {
      for seg in sub {
        match seg {
          PathSeg::Move(x, y) => {
            let (px, py) = self.mapped(*x, *y, ox, oy1, &item.ctm);
            shape.move_to((px as f64, py as f64));
          }
          PathSeg::Line(x, y) => {
            let (px, py) = self.mapped(*x, *y, ox, oy1, &item.ctm);
            shape.line_to((px as f64, py as f64));
          }
          PathSeg::Curve(x1, y1, x2, y2, x3, y3) => {
            let (a, b) = self.mapped(*x1, *y1, ox, oy1, &item.ctm);
            let (c, d) = self.mapped(*x2, *y2, ox, oy1, &item.ctm);
            let (e, f) = self.mapped(*x3, *y3, ox, oy1, &item.ctm);
            shape.curve_to((a as f64, b as f64), (c as f64, d as f64), (e as f64, f as f64));
          }
          PathSeg::Close => shape.close_path(),
        }
      }
    }
    shape
  }

  fn mapped(&self, x: f32, y: f32, ox: f32, oy1: f32, ctm: &crate::graphics::Matrix) -> (f32, f32) {
    let (ux, uy) = ctm.apply(x, y);
    self.map_point(ox, oy1, ux, uy)
  }

  fn render(&mut self, scene: &mut Scene, fonts: &mut FontSystem) {
    let scale = fonts.scale;
    self.ensure_layouts(fonts);
    let (page_w, page_h) = self.page_size();
    scene.fill(
      Fill::NonZero,
      Affine::translate((self.x as f64, self.y as f64)),
      &Brush::Solid(PDF_PAPER),
      None,
      &Rect::new(0.0, 0.0, page_w as f64, page_h as f64),
    );
    let page = match self.doc.page(self.page_no) {
      Ok(page) => page.clone(),
      Err(_) => return,
    };
    let (ox, oy1) = (page.origin_x, page.origin_y + page.height);
    if self.img_page != self.page_no {
      self.images.clear();
      self.img_page = self.page_no;
    }
    let mut run_idx = 0usize;
    let mut clip_depth = 0usize;
    let mut save_stack: Vec<usize> = Vec::new();
    for (item_no, item) in page.items.iter().enumerate() {
      match item {
        PageItem::Text(_) => {
          if let Some(run) = self.runs.get(run_idx) {
            draw_line(
              scene,
              &run.layout,
              run.x,
              run.y,
              CrispOpts { scale, hint: true, subpixel: true },
            );
          }
          run_idx += 1;
        }
        PageItem::Path(path) => {
          let shape = self.path_shape(path, ox, oy1);
          if let Some(rule) = path.clip {
            scene.push_clip_layer(fill_of(rule), Affine::IDENTITY, &shape);
            clip_depth += 1;
          }
          if let Some((rgb, rule)) = &path.fill {
            scene.fill(fill_of(*rule), Affine::IDENTITY, &Brush::Solid(rgba(*rgb, path.fill_alpha)), None, &shape);
          }
          if let Some((rgb, style)) = &path.stroke {
            scene.stroke(
              &vello_stroke(style, self.zoom),
              Affine::IDENTITY,
              &Brush::Solid(rgba(*rgb, path.stroke_alpha)),
              None,
              &shape,
            );
          }
        }
        PageItem::Gradient(shading) => self.paint_gradient(scene, shading, ox, oy1),
        PageItem::Image(placed) => {
          self.paint_image(scene, placed, (self.page_no, item_no), ox, oy1);
        }
        PageItem::Save => save_stack.push(clip_depth),
        PageItem::Restore => {
          if let Some(depth) = save_stack.pop() {
            while clip_depth > depth {
              scene.pop_layer();
              clip_depth -= 1;
            }
          }
        }
        // Tiling patterns render later; skipped items stay silent.
        PageItem::Pattern(_) | PageItem::Skipped(_) => {}
        // Structure markers need no paint.
        PageItem::BeginMarked(_) | PageItem::EndMarked => {}
      }
    }
    while clip_depth > 0 {
      scene.pop_layer();
      clip_depth -= 1;
    }
    self.draw_annotations(scene, &page, ox, oy1);
  }

  fn draw_annotations(&self, scene: &mut Scene, page: &crate::page::PdfPage, ox: f32, oy1: f32) {
    for annot in &page.annotations {
      self.paint_annotation(scene, annot, ox, oy1);
    }
  }

  fn paint_annotation(&self, scene: &mut Scene, annot: &crate::annot::Annotation, ox: f32, oy1: f32) {
    let color = annot.color.unwrap_or(Rgb { r: 0.0, g: 0.0, b: 1.0 });
    let pt = |x: f32, y: f32| {
      let (px, py) = self.map_point(ox, oy1, x, y);
      (px as f64, py as f64)
    };
    match annot.subtype.as_str() {
      "Highlight" => {
        let brush = Brush::Solid(rgba(color, 0.35));
        if annot.quads.is_empty() {
          let (x0, y0) = pt(annot.rect[0], annot.rect[1]);
          let (x1, y1) = pt(annot.rect[2], annot.rect[3]);
          scene.fill(Fill::NonZero, Affine::IDENTITY, &brush, None, &Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)));
        }
        for quad in &annot.quads {
          let mut shape = BezPath::new();
          let (x0, y0) = pt(quad[0], quad[1]);
          shape.move_to((x0, y0));
          for pair in quad.chunks(2).skip(1) {
            let (x, y) = pt(pair[0], pair[1]);
            shape.line_to((x, y));
          }
          shape.close_path();
          scene.fill(Fill::NonZero, Affine::IDENTITY, &brush, None, &shape);
        }
      }
      "Underline" | "StrikeOut" => {
        let brush = Brush::Solid(rgba(color, 1.0));
        let stroke = Stroke::new(1.0 * self.zoom as f64);
        let lines: Vec<(f32, f32, f32, f32)> = if annot.quads.is_empty() {
          let (y, frac) = if annot.subtype == "Underline" { (annot.rect[1], 0.0) } else { (0.0, 0.5) };
          let yy = if annot.subtype == "Underline" { y } else { annot.rect[1] + (annot.rect[3] - annot.rect[1]) * frac };
          vec![(annot.rect[0], yy, annot.rect[2], yy)]
        } else {
          annot
            .quads
            .iter()
            .map(|q| {
              let ys = [q[1], q[3], q[5], q[7]];
              let yy = if annot.subtype == "Underline" {
                ys.iter().fold(f32::INFINITY, |a, b| a.min(*b))
              } else {
                (ys.iter().fold(f32::INFINITY, |a, b| a.min(*b)) + ys.iter().fold(f32::NEG_INFINITY, |a, b| a.max(*b))) / 2.0
              };
              (q[0].min(q[2].min(q[4].min(q[6]))), yy, q[0].max(q[2].max(q[4].max(q[6]))), yy)
            })
            .collect()
        };
        for (x0, y0, x1, y1) in lines {
          let mut shape = BezPath::new();
          shape.move_to(pt(x0, y0));
          shape.line_to(pt(x1, y1));
          scene.stroke(&stroke, Affine::IDENTITY, &brush, None, &shape);
        }
      }
      "Square" | "Circle" => {
        if annot.border_width <= 0.0 {
          return;
        }
        let brush = Brush::Solid(rgba(color, 1.0));
        let stroke = Stroke::new((annot.border_width.max(0.5) * self.zoom) as f64);
        if annot.subtype == "Circle" {
          let (x0, y0) = pt(annot.rect[0], annot.rect[1]);
          let (x1, y1) = pt(annot.rect[2], annot.rect[3]);
          let shape = vello::kurbo::Ellipse::new(
            vello::kurbo::Point::new((x0 + x1) / 2.0, (y0 + y1) / 2.0),
            ((x1 - x0).abs() / 2.0, (y1 - y0).abs() / 2.0),
            0.0,
          );
          scene.stroke(&stroke, Affine::IDENTITY, &brush, None, &shape);
        } else {
          let (x0, y0) = pt(annot.rect[0], annot.rect[1]);
          let (x1, y1) = pt(annot.rect[2], annot.rect[3]);
          scene.stroke(&stroke, Affine::IDENTITY, &brush, None, &Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)));
        }
      }
      "Ink" => {
        let brush = Brush::Solid(rgba(color, 1.0));
        let stroke = Stroke::new(1.0 * self.zoom as f64);
        for line in &annot.ink {
          if line.len() < 2 {
            continue;
          }
          let mut shape = BezPath::new();
          let (x0, y0) = pt(line[0].0, line[0].1);
          shape.move_to((x0, y0));
          for (x, y) in &line[1..] {
            let (px, py) = pt(*x, *y);
            shape.line_to((px, py));
          }
          scene.stroke(&stroke, Affine::IDENTITY, &brush, None, &shape);
        }
      }
      "Link" => {
        if annot.border_width <= 0.0 {
          return;
        }
        let brush = Brush::Solid(rgba(color, 1.0));
        let stroke = Stroke::new((annot.border_width.max(0.5) * self.zoom) as f64);
        let (x0, y0) = pt(annot.rect[0], annot.rect[1]);
        let (x1, y1) = pt(annot.rect[2], annot.rect[3]);
        scene.stroke(&stroke, Affine::IDENTITY, &brush, None, &Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)));
      }
      _ => {}
    }
  }

  fn paint_image(&mut self, scene: &mut Scene, placed: &PlacedImage, key: (usize, usize), ox: f32, oy1: f32) {
    let img = &placed.image;
    if img.width == 0 || img.height == 0 {
      return;
    }
    if !self.images.contains_key(&key) {
      let data = ImageData {
        data: img.rgba.clone().into(),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: img.width,
        height: img.height,
      };
      self.images.insert(key, data);
    }
    let data = match self.images.get(&key) {
      Some(data) => data,
      None => return,
    };
    // Unit square (image space, top row first) through CTM to the page.
    let flip = Affine::new([1.0, 0.0, 0.0, -1.0, 0.0, 1.0]);
    let ctm = Affine::new([
      placed.ctm.a as f64,
      placed.ctm.b as f64,
      placed.ctm.c as f64,
      placed.ctm.d as f64,
      placed.ctm.e as f64,
      placed.ctm.f as f64,
    ]);
    let zoom = self.zoom as f64;
    let tx = (self.x - ox * self.zoom) as f64;
    let ty = (self.y + oy1 * self.zoom) as f64;
    let page = Affine::new([zoom, 0.0, 0.0, -zoom, tx, ty]);
    scene.draw_image(data, page * ctm * flip);
  }

  fn paint_gradient(&self, scene: &mut Scene, shading: &GradientItem, ox: f32, oy1: f32) {
    let stops: Vec<ColorStop> = shading
      .stops
      .iter()
      .map(|(offset, rgb)| ColorStop { offset: offset.clamp(0.0, 1.0), color: rgba(*rgb, 1.0).into() })
      .collect();
    if stops.is_empty() {
      return;
    }
    let map = |x: f32, y: f32| {
      let (ux, uy) = shading.ctm.apply(x, y);
      let (px, py) = self.map_point(ox, oy1, ux, uy);
      Point::new(px as f64, py as f64)
    };
    // Non-extended shadings ideally paint nothing outside [0, 1];
    // Pad approximates the common fully-covered case.
    let gradient = if shading.radial {
      let r1 = shading.coords.get(5).copied().unwrap_or(0.0) * self.zoom;
      if r1 <= 0.0 {
        return;
      }
      Gradient::new_radial(map(shading.coords[3], shading.coords[4]), r1).with_extend(Extend::Pad)
    } else {
      Gradient::new_linear(map(shading.coords[0], shading.coords[1]), map(shading.coords[2], shading.coords[3]))
        .with_extend(Extend::Pad)
    }
    .with_stops(stops.as_slice());
    // Paint across the page; the current clip (if any) bounds it.
    let (page_w, page_h) = self.page_size();
    scene.fill(
      Fill::NonZero,
      Affine::translate((self.x as f64, self.y as f64)),
      &Brush::Gradient(gradient),
      None,
      &Rect::new(0.0, 0.0, page_w as f64, page_h as f64),
    );
  }
}

fn run_rgb(run: &PdfTextRun) -> Rgb {
  Rgb { r: run.color_rgb[0], g: run.color_rgb[1], b: run.color_rgb[2] }
}

/// Measured distance (device px) from the top of a laid-out `CTLine`
/// to its typographic baseline: the first Parley line's baseline
/// offset. Falls back to 80% of the line height when the layout has
/// no lines (never expected for non-empty runs).
fn line_baseline(line: &CTLine) -> f32 {
  if let Some(first) = line.inner().lines().next() {
    let baseline = first.metrics().baseline;
    if baseline.is_finite() && baseline > 0.0 {
      return baseline;
    }
  }
  line.height() * line.scale() * 0.8
}

fn rgba(rgb: Rgb, alpha: f32) -> Color {
  let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
  Color::from_rgba8(
    (rgb.r.clamp(0.0, 1.0) * 255.0) as u8,
    (rgb.g.clamp(0.0, 1.0) * 255.0) as u8,
    (rgb.b.clamp(0.0, 1.0) * 255.0) as u8,
    a,
  )
}
fn fill_of(rule: FillRule) -> Fill {
  match rule {
    FillRule::NonZero => Fill::NonZero,
    FillRule::EvenOdd => Fill::EvenOdd,
  }
}

fn vello_stroke(style: &crate::graphics::StrokeStyle, zoom: f32) -> Stroke {
  let mut stroke = Stroke::new((style.width.max(0.0) * zoom) as f64);
  let cap = match style.cap {
    1 => Cap::Round,
    2 => Cap::Square,
    _ => Cap::Butt,
  };
  stroke.start_cap = cap;
  stroke.end_cap = cap;
  stroke.join = match style.join {
    1 => Join::Round,
    2 => Join::Bevel,
    _ => Join::Miter,
  };
  stroke.miter_limit = style.miter as f64;
  if !style.dash.is_empty() {
    stroke = stroke.with_dashes(
      (style.phase * zoom) as f64,
      style.dash.iter().map(|d| (d * zoom) as f64),
    );
  }
  stroke
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
