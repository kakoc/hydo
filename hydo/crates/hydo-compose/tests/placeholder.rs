//! What the import writes for NSQ's Compose file of the 0.6.0 trial (Milestone 363): the
//! workload's placeholder names `nsqd`, which takes the writes, not `nsqlookupd`, the first
//! service, which only tells the clients where `nsqd` is. A volume whose mount the import leaves
//! out (on `/data`, the data disk's root) gets no folder of `data.dirs`.

use crate::tmpdir::TmpDir;
use hydo_compose::registry::Registry;

struct Refuses;

impl Registry for Refuses {
    fn manifest(&mut self, _: &str, repo: &str, r: &str) -> Result<Vec<u8>, String> {
        Err(format!("no registry here: {repo}:{r}"))
    }
    fn blob(&mut self, _: &str, repo: &str, d: &str) -> Result<Vec<u8>, String> {
        Err(format!("no registry here: {repo}@{d}"))
    }
}

fn import(tag: &str, compose: &str) -> String {
    let d = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-compose-ph-{}-{tag}", std::process::id())),
    );
    std::fs::write(d.join("docker-compose.yml"), compose).unwrap();
    let a = vec![
        d.join("docker-compose.yml").display().to_string(),
        "--no-resolve".into(),
    ];
    assert_eq!(
        hydo_compose::main_with_clock(&a, &mut Refuses, Some(1794096000)),
        0
    );
    std::fs::read_to_string(d.join("hydo.yaml")).unwrap()
}

/// nsq.io's Docker page, with `nsqd`'s data on a volume, as the trial ran it.
const NSQ: &str = "services:
  nsqlookupd:
    image: nsqio/nsq:v1.3.0
    command: /nsqlookupd
    ports: ['4160', '4161']
  nsqd:
    image: nsqio/nsq:v1.3.0
    command: /nsqd --lookupd-tcp-address=nsqlookupd:4160 --data-path=/data --mem-queue-size=0
    depends_on: [nsqlookupd]
    ports: ['4150', '4151']
    volumes: ['nsqd-data:/data']
  nsqadmin:
    image: nsqio/nsq:v1.3.0
    command: /nsqadmin --lookupd-http-address=nsqlookupd:4161
    depends_on: [nsqlookupd]
    ports: ['4171']
volumes:
  nsqd-data: {}
";

#[test]
fn the_placeholder_names_the_service_that_takes_the_writes() {
    let m = import("nsq", NSQ);
    assert!(
        m.contains("# TODO: one operation with item $i against nsqd (port 4150)"),
        "{m}"
    );
    assert!(!m.contains("against nsqlookupd"), "{m}");
}

#[test]
fn a_volume_that_is_not_mounted_gets_no_data_folder() {
    let m = import("nsq-dirs", NSQ);
    assert!(m.contains("on /data"), "the import says why: {m}");
    assert!(!m.contains("vol/nsqd-data"), "{m}");
}

/// An application whose database the Compose file lists first: the placeholder names the
/// application, which depends on the database.
#[test]
fn an_application_comes_before_its_database() {
    let m = import(
        "app",
        "services:
  db:
    image: postgres:16
    ports: ['5432']
    volumes: ['pg:/var/lib/postgresql/data']
  app:
    image: example/app:1
    ports: ['8080']
    depends_on: [db]
volumes:
  pg: {}
",
    );
    assert!(m.contains("against app (port 8080)"), "{m}");
    // the database's volume is mounted, so its folder stays
    assert!(m.contains("vol/pg"), "{m}");
}

/// Milestone 368 (the Typesense onboarding): a fixed `ipv4_address` that hydo's own order of
/// `addr:` gives the service too is kept without a word; another address is still named as
/// ignored, with the address the service gets.
#[test]
fn a_fixed_address_that_is_kept_says_nothing() {
    let d = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-compose-ipv4-{}", std::process::id())),
    );
    let compose = "services:
  ts1:
    image: typesense/typesense:30.1
    networks:
      tsnet:
        ipv4_address: 10.89.0.11
  ts2:
    image: typesense/typesense:30.1
    networks:
      tsnet:
        ipv4_address: 10.89.0.99
networks:
  tsnet: {}
";
    std::fs::write(d.join("docker-compose.yml"), compose).unwrap();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_hydo-compose"))
        .arg(d.join("docker-compose.yml"))
        .arg("--no-resolve")
        .env("HYDO_INIT_CLOCK_START", "2026-11-26")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(!err.contains("ipv4_address 10.89.0.11"), "{err}");
    assert!(
        err.contains("service ts2: ipv4_address 10.89.0.99 ignored; the service is at 10.89.0.12"),
        "{err}"
    );
}
