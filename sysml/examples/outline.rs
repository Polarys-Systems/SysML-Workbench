//! Prints an outline of a SysML file and any diagnostics.
//!
//! cargo run -p sysml --example outline -- path/to/model.sysml

use sysml::*;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: outline <file.sysml>");
        std::process::exit(2);
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    let result = parse(&src);
    result.model.walk(&mut |m, depth| println!("{}{}", "  ".repeat(depth), m.kind.summary()));

    let lines = LineIndex::new(&src);
    for d in &result.diagnostics {
        let (line, col) = lines.line_col(d.span.start);
        eprintln!("{path}:{line}:{col}: {d}");
    }
    if result.has_errors() {
        std::process::exit(1);
    }
}
