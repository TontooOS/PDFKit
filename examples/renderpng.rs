use pdfkit::PdfDocument;
use tontooui::elements::layout::View as UiView;
use tontooui::renderer::images::{ImageCache, ImageLoader};
use tontooui::renderer::text::FontSystem;
use vello::peniko::Color;
use vello::wgpu;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

/// Headless offscreen renderer for the visual compare loop.
///
/// Usage: `renderpng <pdf> <outdir> --scale S`
///
/// Renders every page through `PdfDocument` + `PdfView` (the same
/// item-to-scene code path as the viewer example) into a windowless
/// wgpu texture, then saves `pageN.png` at exactly
/// (mediabox_pts * S) pixels. Runs on Windows (GPU present); build
/// with plain `cargo run --example renderpng` (no WSL needed).
fn main() {
  let args: Vec<String> = std::env::args().collect();
  if args.len() < 3 {
    eprintln!("usage: renderpng <pdf> <outdir> --scale S");
    std::process::exit(2);
  }
  let pdf = &args[1];
  let outdir = &args[2];
  let mut scale = 2.0f32;
  let mut i = 3;
  while i < args.len() {
    if args[i] == "--scale" && i + 1 < args.len() {
      scale = args[i + 1].parse().unwrap_or_else(|_| {
        eprintln!("bad --scale value");
        std::process::exit(2);
      });
      i += 2;
    } else {
      eprintln!("unknown arg {}", args[i]);
      std::process::exit(2);
    }
  }
  if !(0.25..=8.0).contains(&scale) {
    eprintln!("scale {scale} out of range 0.25..=8.0");
    std::process::exit(2);
  }
  std::fs::create_dir_all(outdir).unwrap_or_else(|e| {
    eprintln!("cannot create {outdir}: {e}");
    std::process::exit(1);
  });
  let doc = PdfDocument::load_file(pdf).unwrap_or_else(|e| {
    eprintln!("cannot open '{pdf}': {e}");
    std::process::exit(1);
  });
  let pages = doc.page_count();
  let mut gpu = Gpu::new();
  let mut view = pdfkit::PdfView::new(doc);
  let mut fonts = FontSystem::new();
  // Same separation as the viewer shell: zoom scales the page,
  // fonts.scale is the device pixel ratio of the target.
  fonts.scale = scale;
  view.set_zoom(scale);
  let mut cache = ImageCache::new();
  let mut scene = Scene::new();
  for n in 0..pages {
    view.set_page(n);
    let (lw, lh) = view.measure(&mut fonts);
    let w = lw.round().max(1.0) as u32;
    let h = lh.round().max(1.0) as u32;
    view.place(&mut fonts, 0.0, 0.0, lw, lh);
    scene.reset();
    {
      let mut loader = ImageLoader::new(&mut gpu.renderer, &gpu.device, &gpu.queue, &mut cache);
      view.draw(&mut scene, &mut fonts, &mut loader);
    }
    let rgba = gpu.render_page(&mut scene, w, h);
    let out = format!("{outdir}/page{n}.png");
    write_png(&out, w, h, &rgba).unwrap_or_else(|e| {
      eprintln!("cannot save {out}: {e}");
      std::process::exit(1);
    });
    println!("page{n}: {w}x{h} -> {out}");
  }
  println!("rendered {pages} pages at scale {scale}");
}

struct Gpu {
  device: wgpu::Device,
  queue: wgpu::Queue,
  renderer: Renderer,
}

impl Gpu {
  fn new() -> Self {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
      power_preference: wgpu::PowerPreference::HighPerformance,
      force_fallback_adapter: false,
      compatible_surface: None,
    }))
    .unwrap_or_else(|e| {
      eprintln!("no usable GPU adapter: {e:?}");
      std::process::exit(1);
    });
    let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
      label: Some("renderpng"),
      ..Default::default()
    }))
    .unwrap_or_else(|e| {
      eprintln!("cannot open GPU device: {e:?}");
      std::process::exit(1);
    });
    let renderer = Renderer::new(
      &device,
      RendererOptions { use_cpu: false, antialiasing_support: AaSupport::all(), ..Default::default() },
    )
    .unwrap_or_else(|e| {
      eprintln!("cannot create vello renderer: {e:?}");
      std::process::exit(1);
    });
    Self { device, queue, renderer }
  }

  /// Render `scene` (exactly w x h units) to an RGBA8 buffer.
  fn render_page(&mut self, scene: &mut Scene, w: u32, h: u32) -> Vec<u8> {
    let texture = self.device.create_texture(&wgpu::TextureDescriptor {
      label: Some("renderpng page"),
      size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
      mip_level_count: 1,
      sample_count: 1,
      dimension: wgpu::TextureDimension::D2,
      format: wgpu::TextureFormat::Rgba8Unorm,
      usage: wgpu::TextureUsages::TEXTURE_BINDING
        | wgpu::TextureUsages::STORAGE_BINDING
        | wgpu::TextureUsages::COPY_SRC,
      view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let params = RenderParams {
      base_color: Color::from_rgb8(255, 255, 255),
      width: w,
      height: h,
      antialiasing_method: AaConfig::Msaa8,
    };
    self.renderer.render_to_texture(&self.device, &self.queue, scene, &view, &params).unwrap_or_else(|e| {
      eprintln!("render failed: {e:?}");
      std::process::exit(1);
    });
    // Read back with 256-byte aligned rows, then strip the padding.
    let stride = w * 4;
    let padded = stride.div_ceil(256) * 256;
    let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
      label: Some("renderpng readback"),
      size: (padded * h) as u64,
      usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
      mapped_at_creation: false,
    });
    let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
    encoder.copy_texture_to_buffer(
      wgpu::TexelCopyTextureInfo {
        texture: &texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
      },
      wgpu::TexelCopyBufferInfo {
        buffer: &buffer,
        layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
      },
      wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    self.queue.submit(Some(encoder.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
      let _ = tx.send(r);
    });
    self
      .device
      .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
      .unwrap_or_else(|e| {
        eprintln!("GPU poll failed: {e:?}");
        std::process::exit(1);
      });
    rx.recv().unwrap().unwrap_or_else(|e| {
      eprintln!("buffer map failed: {e:?}");
      std::process::exit(1);
    });
    let mapped = buffer.slice(..).get_mapped_range();
    let mut rgba = Vec::with_capacity((stride * h) as usize);
    for row in 0..h as usize {
      let start = row * padded as usize;
      rgba.extend_from_slice(&mapped[start..start + stride as usize]);
    }
    drop(mapped);
    buffer.unmap();
    rgba
  }
}

/// Lossless RGBA8 PNG writer (filter 0, zlib via ArchiveKit).
/// Local to this example so page renders never depend on the
/// CoreImage PNG encoder; reading still goes through CoreImage.
fn write_png(path: &str, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
  if w == 0 || h == 0 {
    return Err("empty image".into());
  }
  if rgba.len() != w as usize * h as usize * 4 {
    return Err("pixel buffer length mismatch".into());
  }
  let stride = w as usize * 4;
  let mut raw = Vec::with_capacity((stride + 1) * h as usize);
  for y in 0..h as usize {
    raw.push(0);
    raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
  }
  let compressed = archivekit::zlib_compress(&raw, archivekit::CompressionLevel::Balanced);
  let mut out = Vec::with_capacity(compressed.len() + 128);
  out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
  let mut ihdr = Vec::with_capacity(13);
  ihdr.extend_from_slice(&w.to_be_bytes());
  ihdr.extend_from_slice(&h.to_be_bytes());
  ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
  write_chunk(&mut out, b"IHDR", &ihdr);
  write_chunk(&mut out, b"IDAT", &compressed);
  write_chunk(&mut out, b"IEND", &[]);
  std::fs::write(path, &out).map_err(|e| e.to_string())?;
  Ok(())
}

fn crc32(tag: &[u8; 4], data: &[u8]) -> u32 {
  static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
  let table = TABLE.get_or_init(|| {
    let mut t = [0u32; 256];
    for (i, slot) in t.iter_mut().enumerate() {
      let mut c = i as u32;
      for _ in 0..8 {
        c = if c & 1 == 1 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
      }
      *slot = c;
    }
    t
  });
  let mut crc = 0xFFFF_FFFFu32;
  for &b in tag.iter().chain(data.iter()) {
    crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
  }
  crc ^ 0xFFFF_FFFF
}

fn write_chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
  out.extend_from_slice(&(data.len() as u32).to_be_bytes());
  out.extend_from_slice(tag);
  out.extend_from_slice(data);
  out.extend_from_slice(&crc32(tag, data).to_be_bytes());
}

/// Minimal blocking executor (std only): drives a future to
/// completion on this thread. Enough for wgpu's short setup
/// futures without adding an async runtime dependency.
fn block_on<F: std::future::Future>(mut future: F) -> F::Output {
  use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
  unsafe fn clone_waker(_: *const ()) -> RawWaker {
    raw_waker()
  }
  unsafe fn noop(_: *const ()) {}
  fn raw_waker() -> RawWaker {
    const TABLE: RawWakerVTable = RawWakerVTable::new(clone_waker, noop, noop, noop);
    RawWaker::new(std::ptr::null(), &TABLE)
  }
  let waker = unsafe { Waker::from_raw(raw_waker()) };
  let mut cx = Context::from_waker(&waker);
  // SAFETY: the future is never moved after pinning here.
  let mut pinned = unsafe { std::pin::Pin::new_unchecked(&mut future) };
  loop {
    match pinned.as_mut().poll(&mut cx) {
      Poll::Ready(value) => return value,
      Poll::Pending => std::thread::yield_now(),
    }
  }
}
