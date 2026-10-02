use std::any::Any;

use tontooui::elements::layout::View;
use tontooui::renderer::images::ImageLoader;
use tontooui::renderer::text::{CTLine, CrispOpts, FontSystem, draw_line};
use tontooui::theme::ThemeMode;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Join, Point, Rect, Stroke};
use vello::peniko::{Brush, Color, ColorStop, Extend, Fill, Gradient, ImageAlphaType, ImageBrush, ImageData, ImageFormat, ImageQuality};

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

/// One laid-out text run. The position is kept in PDF user space, not
/// view space: the layout cache must survive `place()` moves (scrolling,
/// multi-page stacks) or every page would draw its text at the stale
/// offset from the first `measure()`.
struct RunLayout {
  layout: CTLine,
  /// Baseline origin in PDF user space (CTM already baked in).
  ux: f32,
  uy: f32,
  /// Measured first-line baseline of `layout` in device px.
  baseline: f32,
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
      Ok(page) => {
        let (w, h) = if is_sideways(page.rotate) { (page.height, page.width) } else { (page.width, page.height) };
        (w * self.zoom, h * self.zoom)
      }
      Err(_) => (0.0, 0.0),
    }
  }

  /// User space to view logical px through the page `/Rotate`
  /// transform (y flipped). Every paint path shares this, so rotated
  /// pages land in the reference frame.
  fn page_affine(&self, page: &crate::page::PdfPage) -> Affine {
    let (w, h) = (page.width as f64, page.height as f64);
    // Local (origin-relative, y-up) to display (y-up) rotation.
    let rot = match rotate_rem(page.rotate) {
      90 => Affine::new([0.0, -1.0, 1.0, 0.0, 0.0, w]),
      180 => Affine::new([-1.0, 0.0, 0.0, -1.0, w, h]),
      270 => Affine::new([0.0, 1.0, -1.0, 0.0, h, 0.0]),
      _ => Affine::IDENTITY,
    };
    let zoom = self.zoom as f64;
    let dh = if is_sideways(page.rotate) { w } else { h };
    let view = Affine::new([zoom, 0.0, 0.0, -zoom, self.x as f64, (self.y as f64) + dh * zoom]);
    view * rot * Affine::translate((-(page.origin_x as f64), -(page.origin_y as f64)))
  }

  /// Map a user-space point to view logical px (y flipped).
  fn map_point(&self, page: &crate::page::PdfPage, ux: f32, uy: f32) -> (f32, f32) {
    let p = self.page_affine(page) * Point::new(ux as f64, uy as f64);
    (p.x as f32, p.y as f32)
  }

  /// Draw origin of a run in `draw_line` units. The CoreText pipeline
  /// draws in device px (`fonts.scale` per logical px), while paths and
  /// the paper use zoomed points. The layout is therefore built at
  /// `font_size * zoom / scale` so its device advances land back on
  /// zoomed points, and the draw origin is pre-divided by `scale`
  /// (the draw call re-multiplies). The vertical origin is the PDF
  /// baseline minus the measured first-line baseline of the laid-out
  /// line, not a `font_size` estimate.
  ///
  /// Mapped at draw time from the user-space origin so the layout cache
  /// stays independent of the current `place()` position.
  fn run_draw_origin(&self, run: &RunLayout, page: &crate::page::PdfPage, scale: f32) -> (f32, f32) {
    let (px, py) = self.map_point(page, run.ux, run.uy);
    (px / scale, (py - run.baseline) / scale)
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
        self.runs.push(RunLayout { layout, ux: run.x, uy: run.y, baseline });
      }
    }
    self.layout_scale = fonts.scale;
    self.dirty = false;
  }

  fn path_shape(&self, item: &PathItem, page: &crate::page::PdfPage) -> BezPath {
    let mut shape = BezPath::new();
    for sub in &item.subpaths {
      for seg in sub {
        match seg {
          PathSeg::Move(x, y) => {
            let (px, py) = self.mapped(*x, *y, page, &item.ctm);
            shape.move_to((px as f64, py as f64));
          }
          PathSeg::Line(x, y) => {
            let (px, py) = self.mapped(*x, *y, page, &item.ctm);
            shape.line_to((px as f64, py as f64));
          }
          PathSeg::Curve(x1, y1, x2, y2, x3, y3) => {
            let (a, b) = self.mapped(*x1, *y1, page, &item.ctm);
            let (c, d) = self.mapped(*x2, *y2, page, &item.ctm);
            let (e, f) = self.mapped(*x3, *y3, page, &item.ctm);
            shape.curve_to((a as f64, b as f64), (c as f64, d as f64), (e as f64, f as f64));
          }
          PathSeg::Close => shape.close_path(),
        }
      }
    }
    shape
  }

  fn mapped(&self, x: f32, y: f32, page: &crate::page::PdfPage, ctm: &crate::graphics::Matrix) -> (f32, f32) {
    let (ux, uy) = ctm.apply(x, y);
    self.map_point(page, ux, uy)
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
    if self.img_page != self.page_no {
      self.images.clear();
      self.img_page = self.page_no;
    }
    let mut run_idx = 0usize;
    let mut clip_depth = 0usize;
    let mut save_stack: Vec<usize> = Vec::new();
    for (item_no, item) in page.items.iter().enumerate() {
      match item {
        PageItem::Text(pdf_run) => {
          if let Some(run) = self.runs.get(run_idx) {
            let opts = CrispOpts { scale, hint: true, subpixel: true };
            let (ox, oy) = self.run_draw_origin(run, &page, scale);
            let (ddx, ddy) = page_text_dir(page.rotate, pdf_run.dir_x, pdf_run.dir_y);
            match text_rotation_angle(ddx, ddy) {
              None => draw_line(scene, &run.layout, ox, oy, opts),
              Some(angle) => {
                let (gx, gy) = self.map_point(&page, run.ux, run.uy);
                draw_rotated_line(scene, &run.layout, ox, oy, opts, angle, gx / scale, gy / scale)
              }
            }
          }
          run_idx += 1;
        }
        PageItem::Path(path) => {
          let shape = self.path_shape(path, &page);
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
        PageItem::Gradient(shading) => self.paint_gradient(scene, shading, &page),
        PageItem::Image(placed) => {
          self.paint_image(scene, placed, (self.page_no, item_no), &page);
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
    self.draw_annotations(scene, &page);
  }

  fn draw_annotations(&self, scene: &mut Scene, page: &crate::page::PdfPage) {
    for annot in &page.annotations {
      self.paint_annotation(scene, annot, page);
    }
  }

  fn paint_annotation(&self, scene: &mut Scene, annot: &crate::annot::Annotation, page: &crate::page::PdfPage) {
    // Annotations without /C fall back to black (poppler/Acrobat
    // behavior); the old blue fallback painted colorless link
    // borders blue (coverage E086).
    let color = annot.color.unwrap_or(Rgb::black());
    let pt = |x: f32, y: f32| {
      let (px, py) = self.map_point(page, x, y);
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
      "Text" => {
        // Sticky-note marker: filled note rect in the annot color.
        // Poppler draws a detailed folded-note icon; the fill keeps
        // the marker visible and positioned instead of skipping the
        // annotation (coverage E094). No dark border: the marker must
        // not pollute ink-edge comparison (it is all mid-tone, like
        // the reference icon).
        let brush = Brush::Solid(rgba(color, 1.0));
        let (x0, y0) = pt(annot.rect[0], annot.rect[1]);
        let (x1, y1) = pt(annot.rect[2], annot.rect[3]);
        let shape = Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
        scene.fill(Fill::NonZero, Affine::IDENTITY, &brush, None, &shape);
      }
      _ => {}
    }
  }

  fn paint_image(&mut self, scene: &mut Scene, placed: &PlacedImage, key: (usize, usize), page: &crate::page::PdfPage) {
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
    // PDF default is blocky pixels (`/Interpolate` false); only
    // smooth when the dict asks for interpolation.
    let quality = if placed.image.interpolate { ImageQuality::Medium } else { ImageQuality::Low };
    let brush = ImageBrush::new(data.clone()).with_quality(quality);
    // Vello draws the image rect (0,0,w,h) in pixel space, top row
    // first. Map pixels to the PDF unit square first (u = px/w,
    // v = 1 - py/h), then through the page CTM to user space.
    let unit = image_pixel_to_unit(img.width, img.height);
    let ctm = Affine::new([
      placed.ctm.a as f64,
      placed.ctm.b as f64,
      placed.ctm.c as f64,
      placed.ctm.d as f64,
      placed.ctm.e as f64,
      placed.ctm.f as f64,
    ]);
    scene.draw_image(&brush, self.page_affine(page) * ctm * unit);
  }

  fn paint_gradient(&self, scene: &mut Scene, shading: &GradientItem, page: &crate::page::PdfPage) {
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
      let (px, py) = self.map_point(page, ux, uy);
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

/// Normalized page rotation: 0, 90, 180 or 270 degrees clockwise.
fn rotate_rem(rotate: i32) -> i32 {
  ((rotate % 360) + 360) % 360
}

/// True for 90/270 degree pages (width and height swap on display).
fn is_sideways(rotate: i32) -> bool {
  matches!(rotate_rem(rotate), 90 | 270)
}

/// Run direction (user-space, y-up) mapped to display (y-up) through
/// the page `/Rotate` transform (linear part only: no translation).
fn page_text_dir(rotate: i32, dir_x: f32, dir_y: f32) -> (f32, f32) {
  match rotate_rem(rotate) {
    90 => (dir_y, -dir_x),
    180 => (-dir_x, -dir_y),
    270 => (-dir_y, dir_x),
    _ => (dir_x, dir_y),
  }
}

fn run_rgb(run: &PdfTextRun) -> Rgb {
  Rgb { r: run.color_rgb[0], g: run.color_rgb[1], b: run.color_rgb[2] }
}

/// Rotation angle (device radians) for a run direction, or `None`
/// for plain horizontal text (keeps the fast `draw_line` path).
/// User-space `dir` is y-up; device space is y-down.
fn text_rotation_angle(dir_x: f32, dir_y: f32) -> Option<f64> {
  let angle = (-dir_y as f64).atan2(dir_x as f64);
  if angle.abs() < 0.001 {
    None
  } else {
    Some(angle)
  }
}

/// Draw one laid-out line rotated by `angle` about the glyph origin
/// (`gx`, `gy` draw units: baseline start). Same crisp pipeline as
/// `draw_line` (snapped physical origin, hinted glyphs) plus a rigid
/// Vello glyph transform, so rotated text (`cm` rotation, page
/// `/Rotate`) matches the reference. The pivot is the glyph origin,
/// not the layout origin: the reference rotates typeset glyphs about
/// the text matrix origin.
fn draw_rotated_line(
  scene: &mut Scene,
  line: &CTLine,
  x: f32,
  y: f32,
  opts: CrispOpts,
  angle: f64,
  gx: f32,
  gy: f32,
) {
  use parley::PositionedLayoutItem;
  let scale = opts.scale;
  let (ox, oy) = ((x * scale).round(), (y * scale).round());
  let (px, py) = ((gx * scale).round(), (gy * scale).round());
  let pivot =
    Affine::translate((px as f64, py as f64)) * Affine::rotate(angle) * Affine::translate((-(px as f64), -(py as f64)));
  for parley_line in line.inner().lines() {
    for item in parley_line.items() {
      if let PositionedLayoutItem::GlyphRun(glyph_run) = item {
        let run = glyph_run.run();
        let brush = Brush::Solid(glyph_run.style().brush.color);
        let glyphs = glyph_run.positioned_glyphs().map(|glyph| {
          let (gx, gy) = if opts.subpixel {
            (ox + glyph.x, oy + glyph.y)
          } else {
            ((ox + glyph.x).round(), (oy + glyph.y).round())
          };
          vello::Glyph { id: glyph.id, x: gx, y: gy }
        });
        let mut draw = scene.draw_glyphs(run.font()).font_size(run.font_size());
        if opts.hint {
          draw = draw.hint(true);
        }
        draw.brush(&brush).transform(pivot).draw(Fill::NonZero, glyphs);
      }
    }
  }
}

/// Map image pixels (top row first) to the PDF unit square:
/// top-left `(0,0)` to `(0,1)`, bottom-right `(w,h)` to `(1,0)`.
/// Vello draws the `(0,0,w,h)` pixel rect, while the page CTM maps
/// the unit square, so this adapter sits between them.
fn image_pixel_to_unit(w: u32, h: u32) -> Affine {
  Affine::new([1.0 / w.max(1) as f64, 0.0, 0.0, -1.0 / h.max(1) as f64, 0.0, 1.0])
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

  fn place(&mut self, fonts: &mut FontSystem, x: f32, y: f32, _w: f32, _h: f32) {
    // Position first: every paint path (including the draw-time run
    // origins) reads `self.x`/`self.y`, so they must be current before
    // any layout work happens.
    self.x = x;
    self.y = y;
    self.ensure_layouts(fonts);
    let (page_w, page_h) = self.page_size();
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
  fn run_origins_follow_place_without_rebuild() {
    // Regression: run draw origins were baked into the layout cache at
    // `measure()` time (self.x/self.y were still 0), so a `place()`
    // move left every run painted at the stale offset - all pages of a
    // multi-page stack piled their text on one spot while paths, images
    // and annotations (mapped at draw time) stayed correct.
    let mut fonts = FontSystem::new();
    let mut view = PdfView::new(doc());
    view.measure(&mut fonts);
    let page = view.doc.page(0).unwrap().clone();
    let scale = fonts.scale;
    let before = view.run_draw_origin(&view.runs[0], &page, scale);
    view.place(&mut fonts, 130.0, 417.0, 0.0, 0.0);
    let after = view.run_draw_origin(&view.runs[0], &page, scale);
    // A pure move: the offset shifts by exactly (dx, dy), nothing else.
    assert!((after.0 - before.0 - 130.0).abs() < 1e-3, "{} -> {}", before.0, after.0);
    assert!((after.1 - before.1 - 417.0).abs() < 1e-3, "{} -> {}", before.1, after.1);
  }

  #[test]
  fn stacked_pages_get_distinct_run_origins() {
    // Two views over the same document, stacked like the viewer
    // example: their single runs must land on different baselines.
    let mut fonts = FontSystem::new();
    let mut top = PdfView::new(doc());
    let mut bottom = PdfView::new(doc());
    top.measure(&mut fonts);
    bottom.measure(&mut fonts);
    top.place(&mut fonts, 0.0, 16.0, 0.0, 0.0);
    bottom.place(&mut fonts, 0.0, 16.0 + 792.0 + 24.0, 0.0, 0.0);
    let page = top.doc.page(0).unwrap().clone();
    let scale = fonts.scale;
    let a = top.run_draw_origin(&top.runs[0], &page, scale);
    let b = bottom.run_draw_origin(&bottom.runs[0], &page, scale);
    assert!((b.1 - a.1 - 816.0).abs() < 1e-3, "{} vs {}", a.1, b.1);
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

  #[test]
  fn rotated_run_angle_and_pivot() {
    assert!(text_rotation_angle(1.0, 0.0).is_none());
    let tilt = text_rotation_angle(0.8660254, 0.5).unwrap();
    assert!((tilt + std::f64::consts::FRAC_PI_6).abs() < 1e-6);
    // Rigid rotation about the pivot maps +x onto the device direction.
    let t = Affine::translate((10.0, 20.0)) * Affine::rotate(tilt) * Affine::translate((-10.0, -20.0));
    let p = t * Point::new(11.0, 20.0);
    assert!((p.x - (10.0 + 0.8660254)).abs() < 1e-6);
    assert!((p.y - (20.0 - 0.5)).abs() < 1e-6);
  }

  #[test]
  fn page_rotation_helpers() {
    assert_eq!(rotate_rem(90), 90);
    assert_eq!(rotate_rem(-90), 270);
    assert_eq!(rotate_rem(360), 0);
    assert!(is_sideways(90) && is_sideways(270));
    assert!(!is_sideways(0) && !is_sideways(180));
    // Page /Rotate 90 turns a horizontal run into bottom-to-top text.
    assert_eq!(page_text_dir(90, 1.0, 0.0), (0.0, -1.0));
    assert_eq!(page_text_dir(0, 0.8660, 0.5), (0.8660, 0.5));
  }

  #[test]
  fn image_pixels_map_to_unit_square() {
    // Top-left pixel is unit (0,1), bottom-right is (1,0).
    let t = image_pixel_to_unit(16, 8);
    let p0 = t * Point::new(0.0, 0.0);
    let p1 = t * Point::new(16.0, 8.0);
    assert!((p0.x - 0.0).abs() < 1e-9 && (p0.y - 1.0).abs() < 1e-9);
    assert!((p1.x - 1.0).abs() < 1e-9 && (p1.y - 0.0).abs() < 1e-9);
  }
}
