//! `hydo-fetch-image <registry>/<repository> <sha256:manifest digest | linux/amd64 | linux/arm64>
//! <tag> <out.tar>`: copy ONE image from a registry into a `docker save` archive, without skopeo
//! (the Rust port of hydo/onboard/fetch_image.py). The digest must be a single-platform manifest
//! (hydo-compose.lock.json records the linux/amd64 and linux/arm64 ones). With a platform instead
//! of a digest, the tag of `<tag>` is resolved now (an image `hydo compose --no-resolve` did not
//! pin; the tag can move). Every blob is checked against its digest; the archive's tar headers
//! are fixed (mode 0644, owner 0, time 0). fetch-images.sh, which `hydo compose` writes, calls it.
//! `hydo-fetch-image build ...` builds the image of a Compose `build:` service with Docker and
//! checks it against the lock file's pin (`hydo_compose::localbuild`). `hydo-fetch-image docker
//! ...` saves an image of the local Docker that `hydo compose --from-docker` pinned, and checks it
//! the same way (Milestone 320). A private registry's credential comes from Docker's config
//! (`hydo_compose::creds`).

const USAGE: &str = "fetch_image -- copy ONE image from a Docker registry by manifest digest into a `docker save`
style archive, without skopeo.

    hydo-fetch-image <registry>/<repository> <sha256:manifest digest> <tag recorded in RepoTags> <out.tar>
    e.g. hydo-fetch-image docker.io/library/redis sha256:16b1... redis:alpine images/redis-alpine-amd64.docker.tar
    hydo-fetch-image <registry>/<repository> linux/amd64|linux/arm64 <name:tag> <out.tar>
                     (not pinned: resolves the tag now)
    hydo-fetch-image build ...
                     (builds a Compose `build:` service's image with Docker; `build` alone
                     prints its usage)
    hydo-fetch-image docker ...
                     (saves an image that `hydo compose --from-docker` took from the local
                     Docker; `docker` alone prints its usage)

A registry that asks for a login gets the credential that `docker login` keeps for it, found as
Docker finds it in $DOCKER_CONFIG/config.json (default ~/.docker/config.json): an auths entry, or
the credential helper that credHelpers or credsStore names. No secret is printed.
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
    if a.first().map(String::as_str) == Some("build") {
        // a Compose `build:` service's image (M214): hydo_compose::localbuild
        std::process::exit(hydo_compose::localbuild::build_main(&a[1..]));
    }
    if a.first().map(String::as_str) == Some("docker") {
        // an image that hydo compose --from-docker took from the local Docker (Milestone 320)
        std::process::exit(hydo_compose::localbuild::docker_main(&a[1..]));
    }
    // a private registry's credential, where Docker keeps it (Milestone 320)
    let cfg = hydo_config::Config::process();
    hydo_compose::registry::set_docker_config(cfg.tools.docker_config.clone());
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
