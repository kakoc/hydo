//! hydo-elf-comment FILE [LINE]: sets the "Linker:" line of FILE's .comment section to LINE
//! (default "Linker: LLD"), in place (hydo_tools::elfcomment; hydo/grpcgen/build-client.sh).

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.is_empty() || a.len() > 2 {
        hydo_tools::die("usage: hydo-elf-comment FILE [LINE]");
    }
    let line = a.get(1).map(String::as_str).unwrap_or("Linker: LLD");
    let b = std::fs::read(&a[0]).unwrap_or_else(|e| hydo_tools::die(&format!("{}: {e}", a[0])));
    let out = hydo_tools::elfcomment::fix(&b, line)
        .unwrap_or_else(|e| hydo_tools::die(&format!("{}: {e}", a[0])));
    if out != b {
        std::fs::write(&a[0], out).unwrap_or_else(|e| hydo_tools::die(&format!("{}: {e}", a[0])));
    }
}
