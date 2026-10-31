//! The reader rule, one test per clause (hydo_results::reader's module doc): what a consumer of
//! hydo's results must do, and what hydo's own commands do.

use hydo_results::reader::{document, stream, Record};

const RUN: &str = r#"{"v":1,"type":"run","schedule":"disk=none | run","verdict":{"kind":"ok","text":"HYDO-ORACLE: OK acked=20"},"fingerprints":{"fp":"c513f10aee733aae"}}"#;

#[test]
fn unknown_fields_are_ignored() {
    let line = RUN.replace(
        r#""schedule""#,
        r#""a_field_from_later":{"x":[1,2]},"schedule""#,
    );
    let line = line.replace(
        r#""kind":"ok""#,
        r#""kind":"ok","why":"a field added inside a known object""#,
    );
    let Record::Run(r) = document(&line).unwrap() else {
        panic!("a run")
    };
    assert_eq!(r.schedule, "disk=none | run");
    assert_eq!(r.fingerprints.fp, "c513f10aee733aae");
}

#[test]
fn unknown_record_types_are_skipped_and_listed() {
    let text = format!(
        "{RUN}\n{}\n{RUN}\n",
        r#"{"v":1,"type":"a-record-from-later","anything":true}"#
    );
    let s = stream(&text).unwrap();
    assert_eq!(s.records.len(), 2);
    assert_eq!(s.skipped.len(), 1);
    assert_eq!(s.skipped[0].line, 2);
    assert_eq!(s.skipped[0].ty, "a-record-from-later");
}

#[test]
fn records_of_another_version_are_skipped_in_a_stream() {
    let v2 = RUN.replace(r#"{"v":1,"#, r#"{"v":2,"#);
    let s = stream(&format!("{v2}\n{RUN}\n")).unwrap();
    assert_eq!(s.records.len(), 1);
    assert_eq!((s.skipped[0].v, s.skipped[0].ty.as_str()), (2, "run"));
}

#[test]
fn a_document_of_another_version_is_an_error_that_names_it() {
    let v2 = RUN.replace(r#"{"v":1,"#, r#"{"v":2,"#);
    let e = document(&v2).unwrap_err();
    assert!(e.0.contains("results version 2"), "{e}");
}

#[test]
fn a_document_of_an_unknown_type_is_an_error() {
    let e = document(r#"{"v":1,"type":"a-document-from-later"}"#).unwrap_err();
    assert!(e.0.contains("a-document-from-later"), "{e}");
}

#[test]
fn what_is_not_a_record_is_an_error() {
    for bad in [
        r#"{"type":"run"}"#,
        r#"{"v":1}"#,
        r#"{"v":"1","type":"run"}"#,
        r#"[1,2]"#,
    ] {
        assert!(document(bad).is_err(), "{bad}");
        assert!(stream(&format!("{bad}\n")).is_err(), "{bad}");
    }
    assert!(stream("not json\n").is_err());
}

#[test]
fn a_known_field_of_the_wrong_type_is_an_error() {
    let bad = RUN.replace(r#""schedule":"disk=none | run""#, r#""schedule":12"#);
    let e = stream(&format!("{bad}\n")).unwrap_err();
    assert!(e.0.contains("line 1"), "{e}");
}

#[test]
fn a_last_line_without_its_newline_is_ignored() {
    let cut = &RUN[..RUN.len() / 2];
    let s = stream(&format!("{RUN}\n{cut}")).unwrap();
    assert_eq!(s.records.len(), 1);
    assert!(s.incomplete_tail);
    // a complete last line without a newline is still a cut-off line: the writer ends every line
    let s = stream(&format!("{RUN}\n{RUN}")).unwrap();
    assert_eq!(s.records.len(), 1);
}

#[test]
fn optional_fields_may_be_absent() {
    let Record::Run(r) = document(RUN).unwrap() else {
        panic!("a run")
    };
    assert!(r.findings.is_empty() && r.tag.is_none() && r.image.is_none());
}

#[test]
fn what_hydo_writes_it_reads() {
    let mut r = hydo_results::Run::new(
        "disk=cut | run C=12",
        hydo_results::Verdict {
            kind: "violation".into(),
            text: "HYDO-ORACLE: VIOLATION lost k:2".into(),
            ..Default::default()
        },
        "ecef6f8fe6c39ec0",
    );
    r.findings.push(hydo_results::Finding {
        kind: hydo_results::kind::INVARIANT.into(),
        id: "VIOLATION lost k:#".into(),
        status: hydo_results::status::VIOLATION.into(),
        message: "HYDO-ORACLE: VIOLATION lost k:2".into(),
        ..Default::default()
    });
    let line = hydo_results::to_line(&r);
    assert!(line.starts_with(r#"{"v":1,"type":"run","#), "{line}");
    let s = stream(&line).unwrap();
    assert_eq!(s.records, vec![Record::Run(Box::new(r.clone()))]);
    assert_eq!(
        document(&hydo_results::to_document(&r)).unwrap(),
        Record::Run(Box::new(r))
    );
}
