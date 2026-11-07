//! `kernels::find` asks the registered source (Milestone 162) after the directories, and its
//! refusal says why the source could not help. Its own test program, because a source is
//! registered once per process.

use hydo_image::kernels::{self, Source};
use hydo_image::FileRef;
use std::path::PathBuf;

struct Fake {
    dir: PathBuf,
}

impl Source for Fake {
    fn obtain(&self, want: &FileRef) -> Result<PathBuf, String> {
        let p = self.dir.join("from-the-cache");
        if kernels::sha256_of(&p).ok().as_deref() == Some(want.sha256.as_str()) {
            Ok(p)
        } else {
            Err("no release shipped it (a test)".into())
        }
    }
}

#[test]
fn the_registered_source_is_asked_after_the_directories() {
    let d = std::env::temp_dir().join(format!("hydo-image-obtain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let img = d.join("explore-ob-x-arm64.cpio.gz");
    let mut rec = kernels::record_for("arm64", false);
    // before a source is registered: the refusal of M155, nothing added
    rec.kernel.sha256 = "ab".repeat(32);
    let e = kernels::find(&img, &rec, &[], None).unwrap_err();
    assert!(!e.contains("install cache"), "{e}");
    assert!(kernels::set_source(Box::new(Fake { dir: d.clone() })));
    assert!(!kernels::set_source(Box::new(Fake { dir: d.clone() })));
    std::fs::write(d.join("from-the-cache"), b"a kernel from the cache").unwrap();
    rec.kernel.sha256 = kernels::sha256_of(&d.join("from-the-cache")).unwrap();
    let f = kernels::find(&img, &rec, &[], None).unwrap();
    assert_eq!(f.kernel, d.join("from-the-cache"));
    // a directory that has it wins
    std::fs::write(d.join("Image-explore"), b"a kernel from the cache").unwrap();
    let f = kernels::find(&img, &rec, std::slice::from_ref(&d), None).unwrap();
    assert_eq!(f.kernel, d.join("Image-explore"));
    // neither: the refusal says why the cache could not provide it
    rec.kernel.sha256 = "cd".repeat(32);
    let e = kernels::find(&img, &rec, std::slice::from_ref(&d), None).unwrap_err();
    assert!(
        e.contains("rebuild the image with this hydo (hydo build). The install cache could not provide it: no release shipped it (a test)"),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&d);
}
