//! The permanent examples (`hydo/schema/examples/image/v1/`, recorded from real builds) must
//! parse forever: `image-*.json` as image records (and be canonical JSON), `build-*.json` as build
//! records whose copy of the image record is canonical too.

use std::path::Path;

#[test]
fn every_example_reads() {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/examples/image/v1");
    let mut images = 0;
    let mut builds = 0;
    let mut names: Vec<_> = std::fs::read_dir(&d)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    names.sort();
    for p in names {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&p).unwrap();
        if name.starts_with("image-") {
            let r = hydo_image::parse_record(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(hydo_image::canonical(&r), text, "{name} is not canonical");
            images += 1;
        } else if name.starts_with("build-") {
            let b = hydo_image::parse_build(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(hydo_image::document(&b), text, "{name}");
            assert_eq!(b.record.epoch, 1, "{name}");
            builds += 1;
        } else {
            panic!("{name}: neither image-*.json nor build-*.json");
        }
    }
    assert!(
        images >= 1 && builds >= 1,
        "examples missing in {}",
        d.display()
    );
}
