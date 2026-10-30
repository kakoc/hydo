//! `hydo-fetch-image <registry>/<repository> <sha256:manifest digest> <tag> <out.tar>`: copy ONE
//! image from a registry by manifest digest into a `docker save` archive, without skopeo (the
//! Rust port of hydo/onboard/fetch_image.py). The digest must be a single-platform manifest
//! (hydo-compose.lock.json records the linux/amd64 and linux/arm64 ones). Every blob is checked
//! against its digest; the archive's tar headers are fixed (mode 0644, owner 0, time 0).

const USAGE: &str = "fetch_image -- copy ONE image from a Docker registry by manifest digest into a `docker save`
style archive, without skopeo.

    hydo-fetch-image <registry>/<repository> <sha256:manifest digest> <tag recorded in RepoTags> <out.tar>
    e.g. hydo-fetch-image docker.io/library/redis sha256:16b1... redis:alpine images/redis-alpine-amd64.docker.tar
";

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() != 4 {
        eprint!("{USAGE}");
        std::process::exit(1);
    }
    if let Err(e) = hydo_compose::registry::fetch_image(&a[0], &a[1], &a[2], &a[3]) {
        eprintln!("fetch_image: {e}");
        std::process::exit(1);
    }
}
