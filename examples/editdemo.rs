use pdfkit::{PdfDocument, PdfEditor};

/// Headless edit demo: rewrite one text run and save.
///
/// Usage:
///   cargo run --example editdemo -- <in.pdf> <out.pdf> <page> <run> <new text>
///   cargo run --example editdemo -- <in.pdf> <out.pdf> find <old text> <new text>
///
/// Writes an incremental update: the input file is not modified and the
/// output keeps every original byte, with the changed content stream
/// appended. Verify the result with any reader:
///
///   pdftotext <out.pdf> -        # poppler
fn main() {
  let args: Vec<String> = std::env::args().skip(1).collect();
  if args.len() < 5 {
    eprintln!("usage: editdemo <in.pdf> <out.pdf> <page|find> <run|old text> <new text>");
    eprintln!("   page is zero-based; 'find' replaces the first run with that text");
    std::process::exit(2);
  }
  let (input, output) = (&args[0], &args[1]);
  let new_text = args[4].replace("\\n", "\n");

  let bytes = match std::fs::read(input) {
    Ok(bytes) => bytes,
    Err(e) => {
      eprintln!("cannot read {input}: {e}");
      std::process::exit(1);
    }
  };
  let doc = match PdfDocument::load_bytes(bytes) {
    Ok(doc) => doc,
    Err(e) => {
      eprintln!("cannot parse {input}: {e}");
      std::process::exit(1);
    }
  };
  let original_len = doc.bytes().len();
  let before = match doc.page(0) {
    Ok(page) => page.text(),
    Err(_) => String::new(),
  };

  let mut editor = match PdfEditor::new(doc) {
    Ok(editor) => editor,
    Err(e) => {
      eprintln!("cannot edit {input}: {e}");
      std::process::exit(1);
    }
  };

  let result = if args[2] == "find" {
    editor.replace_text(0, &args[3], &new_text)
  } else {
    let page: usize = match args[2].parse() {
      Ok(page) => page,
      Err(_) => {
        eprintln!("page must be a number or 'find'");
        std::process::exit(2);
      }
    };
    let run: usize = match args[3].parse() {
      Ok(run) => run,
      Err(_) => {
        eprintln!("run must be a number or the text to find");
        std::process::exit(2);
      }
    };
    editor.set_text(page, run, &new_text)
  };
  if let Err(e) = result {
    eprintln!("edit failed: {e}");
    std::process::exit(1);
  }

  // The edited document is available before saving, so a viewer can
  // show the result right away.
  let preview = editor.document().map(|d| d.page(0).ok().map(|p| p.text()).unwrap_or_default()).unwrap_or_default();
  let changed: Vec<String> = preview.lines().map(|l| l.to_owned()).collect();
  // Capture before saving: saving adopts the result and clears the
  // pending-edit counters.
  let edits = editor.edit_count();
  let pages = editor.document().map(|d| d.page_count()).unwrap_or(0);

  if let Err(e) = editor.save_to(output) {
    eprintln!("save failed: {e}");
    std::process::exit(1);
  }
  let saved = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);

  println!("source     {input}");
  println!("pages      {pages}");
  println!("edits      {edits}");
  println!("written    {output}");
  println!("size       {original_len} -> {saved} bytes (incremental, source untouched)");
  println!();
  println!("page 1 before:");
  for line in before.lines() {
    println!("  {line}");
  }
  println!("page 1 after:");
  for line in changed.iter() {
    println!("  {line}");
  }
  println!();
  println!("verify with: pdftotext {output} -");
}