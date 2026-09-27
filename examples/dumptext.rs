use pdfkit::PdfDocument;

/// Debug helper: dump text runs with font, size, position and bytes.
///
/// Usage: `cargo run --example dumptext -- /path/to/file.pdf [page]`
fn main() {
  let path = std::env::args().nth(1).unwrap_or_default();
  if path.is_empty() {
    eprintln!("usage: dumptext <file.pdf> [page]");
    std::process::exit(2);
  }
  let only: Option<usize> = std::env::args().nth(2).and_then(|s| s.parse().ok());
  let doc = PdfDocument::load_file(&path).unwrap();
  println!("pages: {}", doc.page_count());
  for i in 0..doc.page_count() {
    if only.is_some_and(|p| p != i) {
      continue;
    }
    let page = doc.page(i).unwrap();
    println!("--- page {i} {}x{} runs={} items={} annots={}", page.width, page.height, page.runs.len(), page.items.len(), page.annotations.len());
    for run in &page.runs {
      let bytes: Vec<String> = run.text.bytes().map(|b| format!("{b:02X}")).collect();
      println!(
        "  [{} {:.1}pt @ ({:.1},{:.1}) bold={} italic={}] {:?} {}",
        run.font_name, run.font_size, run.x, run.y, run.bold, run.italic, run.text, bytes.join(" ")
      );
    }
  }
}
