use pdfkit::{PageItem, PdfDocument};

fn main() {
  let path = std::env::args().nth(1).expect("pdf path");
  let doc = PdfDocument::load_file(&path).expect("load");
  let page = doc.page(0).unwrap();
  let mut n = 0;
  for item in &page.items {
    match item {
      PageItem::Path(p) => {
        let rule = match &p.fill {
          Some((_, r)) => format!("{r:?}"),
          None => String::from("nofill"),
        };
        println!("DUMPP nsub={} rule={} hasstroke={}", p.subpaths.len(), rule, p.stroke.is_some());
        n += 1;
      }
      PageItem::Text(_) => println!("DUMPTEXT"),
      PageItem::Save => println!("DUMPSAVE"),
      PageItem::Restore => println!("DUMPRESTORE"),
      _ => println!("DUMPOTHER"),
    }
  }
  println!("DUMPTOTAL paths={n}");
}
