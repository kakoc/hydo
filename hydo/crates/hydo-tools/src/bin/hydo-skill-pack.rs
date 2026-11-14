//! hydo-skill-pack PLUGIN_DIR OUT_DIR: the onboarding skill's release assets (Milestone 170; the
//! plugin folder plugin/ since Milestone 229), `OUT_DIR/hydo-skill-<version>.tar.gz`
//! (hydo_agents::release::pack: sorted entries, every time
//! 0, owner 0:0, modes 0644 or 0755, gzip without a name or time, so the same folder gives the same
//! bytes on any machine) and `OUT_DIR/SHA256SUMS`, which `hydo skill update` checks the archive
//! against. Prints the version and the sha256. hydo/dist/skill/build-skill-release.sh runs it.

use sha2::{Digest, Sha256};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() != 2 {
        hydo_tools::die("usage: hydo-skill-pack PLUGIN_DIR OUT_DIR");
    }
    let (dir, out) = (std::path::Path::new(&a[0]), std::path::Path::new(&a[1]));
    let (archive, version) =
        hydo_agents::release::pack(dir).unwrap_or_else(|e| hydo_tools::die(&e.0));
    let name = hydo_agents::release::archive_name(&version);
    let sha: String = Sha256::digest(&archive)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    std::fs::create_dir_all(out).unwrap_or_else(|e| hydo_tools::die(&format!("{}: {e}", a[1])));
    let w = |n: &str, b: &[u8]| {
        let p = out.join(n);
        std::fs::write(&p, b).unwrap_or_else(|e| hydo_tools::die(&format!("{}: {e}", p.display())));
    };
    w(&name, &archive);
    w("SHA256SUMS", format!("{sha}  {name}\n").as_bytes());
    println!("{version} {sha}  {name}");
}
