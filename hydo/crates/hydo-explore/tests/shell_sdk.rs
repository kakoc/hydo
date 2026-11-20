//! The shell assertion SDK (`hydo/stack/hydoassert/hydo_assert.sh`, Milestone 329) and a raw
//! `printf 'HYDO-ASSERT {...}'` line reach the property table that `result.json` carries. The
//! test runs the SDK's functions with this machine's `sh`, `sed`, `awk` and `cut` in place of
//! the guest's busybox (`BB=env`), writes their records to a file (`HYDO_ASSERT_OUT`), and reads
//! the file as a run's console.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The test's folder, removed when the test ends, also when it fails.
struct Gone(PathBuf);

impl Drop for Gone {
    fn drop(&mut self) {
        if std::env::var_os("HYDO_KEEP_TMP").is_none() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[test]
fn the_shell_sdk_s_records_and_a_raw_line_reach_the_property_table() {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../stack/hydoassert/hydo_assert.sh");
    let dir =
        Gone(std::env::temp_dir().join(format!("hydo-explore-shell-sdk-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let out = dir.0.join("console");
    let script = format!(
        r#". '{}'
hydo_decl always shop.order_kept "an acknowledged order is still there"
hydo_always shop.order_kept "an acknowledged order is still there" true
hydo_sometimes shop.retry_won "a \"retry\" succeeded" true
hydo_sometimes shop.never "a branch that never ran" false
hydo_reachable shop.recovered "the recovery ran"
hydo_unreachable shop.double "an order was charged twice"
hydo_eventually shop.settled "orders settle" 5 1 true
printf 'HYDO-ASSERT {{"t":"sometimes","id":"raw.line","ok":true,"msg":"a raw console line"}}\n' >> '{}'
"#,
        sdk.display(),
        out.display()
    );
    let st = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .env("BB", "env")
        .env("HYDO_ASSERT_OUT", &out)
        .status()
        .unwrap();
    // the last call, hydo_unreachable, returns 0; a failed `always` would not
    assert!(st.success(), "{st}");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(
        text.contains(
            r#"{"t":"sometimes","id":"shop.retry_won","ok":true,"msg":"a \"retry\" succeeded"}"#
        ),
        "{text}"
    );
    let t = hydo_explore::results::property_table(std::slice::from_ref(&text))
        .expect("a property table");
    let got: Vec<(String, String, bool, u64)> = t
        .properties
        .iter()
        .map(|p| (p.id.clone(), p.verdict.clone(), p.declared, p.passes))
        .collect();
    let want: Vec<(String, String, bool, u64)> = [
        ("shop.order_kept", "PASS", true, 1),
        ("shop.retry_won", "PASS", false, 1),
        ("shop.recovered", "PASS", false, 1),
        ("shop.double", "FAIL (reached)", false, 0),
        ("shop.settled", "PASS", false, 1),
        ("raw.line", "PASS", false, 1),
    ]
    .iter()
    .map(|(a, b, c, d)| (a.to_string(), b.to_string(), *c, *d))
    .collect();
    assert_eq!(got, want, "{text}");
    assert_eq!(t.violations, 1);
    // HYDO_ASSERT_OUT=off writes nothing
    let off = Command::new("sh")
        .arg("-c")
        .arg(format!(". '{}'; hydo_reachable x y", sdk.display()))
        .env("BB", "env")
        .env("HYDO_ASSERT_OUT", "off")
        .output()
        .unwrap();
    assert!(off.stdout.is_empty());
}
