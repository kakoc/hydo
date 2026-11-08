//! The guest settings every boot path reads from an image (M207, AGENTS.md open item 76).
//!
//! A worker pool and a fresh boot must boot the same guest: `hydo run` read the manifest with the
//! test composer's plan meta (`/etc/hydo/compose.json`), hydo-bench read it without the meta. For
//! a composed plan (a `test:` section whose commands are in the `hydo/` folder) that reading
//! failed, hydo-bench fell back to the defaults of an image without a manifest, and its pools
//! booted at the fingerprint epoch 2 without the CPUID cache leaves, so four recorded schedules
//! gave another fingerprint from the pool than from a fresh boot. Now every path reads
//! [`hydo_control::agent::image_guest`]; these tests pin that reading on the composed examples.

use crate::support::top;
use hydo_control::agent::guest_of_embedded;

/// The composed examples whose manifests are read with their plan meta.
const COMPOSED: &[&str] = &[
    "redis-composed",
    "redis-composed-hist",
    "redis-composed-read",
    "redis-errors",
];

/// The manifest text and the plan meta hydo-build stages for an example's `hydo/hydo.yaml`.
fn staged(ex: &str) -> (String, String) {
    let p = top().join(format!("hydo/onboard/examples/{ex}/hydo/hydo.yaml"));
    let text = std::fs::read_to_string(&p).unwrap();
    let (_, meta) = hydo_manifest::for_explorer(&p).unwrap();
    let meta = meta.unwrap_or_else(|| panic!("{ex}: no plan meta"));
    (text, serde_json::to_string(&meta).unwrap())
}

#[test]
fn a_composed_plan_needs_its_meta() {
    for ex in COMPOSED {
        let (text, _) = staged(ex);
        // the reading hydo-bench's pools did before M207: it fails, and the pool booted with the
        // defaults of an image without a manifest
        assert!(
            hydo_manifest::from_embedded(&text, None).is_err(),
            "{ex}: the manifest reads without its plan meta; the test no longer shows the trap"
        );
        assert!(
            guest_of_embedded(&text, None, &[]).is_err(),
            "{ex}: an unreadable manifest must be an error, not the defaults"
        );
    }
}

#[test]
fn the_guest_of_a_composed_plan() {
    for ex in COMPOSED {
        let (text, meta) = staged(ex);
        let g = guest_of_embedded(&text, Some(&meta), &[]).unwrap_or_else(|e| panic!("{ex}: {e}"));
        // the explorer's reading of the same manifest (hydo explore --manifest)
        let p = top().join(format!("hydo/onboard/examples/{ex}/hydo/hydo.yaml"));
        let (data, _) = hydo_manifest::for_explorer(&p).unwrap();
        let want = hydo_manifest::Guest::from_value(&data);
        assert_eq!(g, want, "{ex}");
        // none of them says guest.cpu_cache: the image's epoch decides (on at epoch 2)
        assert_eq!(g.cpu_cache, None, "{ex}");
        assert_eq!((g.ram_mib, g.cpu.as_str()), (1024, "v2"), "{ex}");
    }
}

#[test]
fn a_key_of_a_composed_plan_is_seen() {
    let (text, meta) = staged("redis-composed");
    let text = format!("{text}\nguest:\n  ram_mib: 2048\n  cpu_cache: false\n");
    let g = guest_of_embedded(&text, Some(&meta), &[]).unwrap();
    assert_eq!((g.ram_mib, g.cpu_cache), (2048, Some(false)));
    // a bad meta is named
    let e = guest_of_embedded(&text, Some("{"), &[]).unwrap_err();
    assert!(e.contains("compose.json"), "{e}");
}
