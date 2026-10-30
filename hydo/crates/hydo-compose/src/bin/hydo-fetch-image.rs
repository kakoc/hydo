//! `hydo-fetch-image <registry>/<repository> <sha256:manifest digest | linux/amd64 | linux/arm64>
//! <tag> <out.tar>`: copy ONE image from a registry into a `docker save` archive, without skopeo
//! (the Rust port of hydo/onboard/fetch_image.py). The digest must be a single-platform manifest
//! (hydo-compose.lock.json records the linux/amd64 and linux/arm64 ones). With a platform instead
//! of a digest, the tag of `<tag>` is resolved now (an image `hydo compose --no-resolve` did not
//! pin; the tag can move). Every blob is checked against its digest; the archive's tar headers
//! are fixed (mode 0644, owner 0, time 0). fetch-images.sh, which `hydo compose` writes, calls it.

const USAGE: &str = "fetch_image -- copy ONE image from a Docker registry by manifest digest into a `docker save`
style archive, without skopeo.

    hydo-fetch-image <registry>/<repository> <sha256:manifest digest> <tag recorded in RepoTags> <out.tar>
    e.g. hydo-fetch-image docker.io/library/redis sha256:16b1... redis:alpine images/redis-alpine-amd64.docker.tar
    hydo-fetch-image <registry>/<repository> linux/amd64|linux/arm64 <name:tag> <out.tar>
                     (not pinned: resolves the tag now)
";

/// The digest of the platform's manifest that `<repo>:<tag of name_tag>` names now.
fn resolve(repo: &str, platform: &str, name_tag: &str) -> Result<String, String> {
    let arch = platform.strip_prefix("linux/").unwrap_or("");
    let last = name_tag.rsplit('/').next().unwrap_or(name_tag);
    let tag = last.split_once(':').map(|(_, t)| t).unwrap_or("latest");
    let r = format!("{repo}:{tag}");
    let mut http = hydo_compose::registry::Http::new();
    let info = hydo_compose::registry::resolve_image(&mut http, &r).map_err(|e| e.0)?;
    match info.getn("platforms").get(arch).map(|p| p.getn("digest")) {
        Some(d) if d.truthy() => Ok(hydo_compose::pyval::pstr(d)),
        _ => Err(format!("{r}: the registry has no {platform} image")),
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() != 4 {
        eprint!("{USAGE}");
        std::process::exit(1);
    }
    let digest = if a[1] == "linux/amd64" || a[1] == "linux/arm64" {
        match resolve(&a[0], &a[1], &a[2]) {
            Ok(d) => {
                eprintln!("fetch_image: {} {} is {d} now (not pinned)", a[0], a[1]);
                d
            }
            Err(e) => {
                eprintln!("fetch_image: {e}");
                std::process::exit(1);
            }
        }
    } else {
        a[1].clone()
    };
    if let Err(e) = hydo_compose::registry::fetch_image(&a[0], &digest, &a[2], &a[3]) {
        eprintln!("fetch_image: {e}");
        std::process::exit(1);
    }
}
