//! The schedule language by example: what is accepted (and prints back byte for byte), what the
//! host side normalises, what is refused, and the hand-written schedules of the documentation.
//! These examples, with the explorer corpus (hydo-explore's tests/corpus.rs), are the language's specification.

use hydo_schedule::*;
use std::path::Path;

fn round_trip(s: &str) {
    let p = Schedule::parse(s).unwrap_or_else(|e| panic!("refused {s:?}: {e}"));
    assert_eq!(p.to_string(), s, "printed differently");
    assert_eq!(Schedule::parse(&p.to_string()).unwrap(), p);
}

fn refused(s: &str, why: &str) {
    match Schedule::parse(s) {
        Ok(p) => panic!("accepted {s:?} (printed {p}); expected a refusal containing {why:?}"),
        Err(e) => assert!(
            e.message.contains(why),
            "{s:?}: message {:?} does not contain {why:?}",
            e.message
        ),
    }
}

/// The committed sample: about 1,700 strings from the explorer corpus, chosen so that every token
/// kind and every rule appears (picked from corpus.py's output before M107, by pattern, then every 900th line).
#[test]
fn sample_round_trips() {
    let text = include_str!("sample.txt");
    let mut n = 0;
    for line in text.lines() {
        round_trip(line);
        n += 1;
    }
    assert!(n > 1000, "{n}");
}

#[test]
fn hand_written_forms() {
    for s in [
        "disk=none | run",
        "disk=cut | run C=12",
        "disk=cut | run C=12 R=everysec",
        "disk=cut | run C=12 S=never",
        "disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24",
        "disk=none dseed=1 | run K=etcd1@10+1 P=etcd3@26+1",
        "disk=none dseed=890abaea1c4d92d3 | run K=etcd1@10+1 P=etcd3@26+1 T=2:-30",
        "disk=none dseed=e80bd3fbb759c1c1 | run K=redis@3+3 N=kafka@21-25:delay,20ms,reorder,11% P=kafka@16+1 T=21:15768000 Q=2d",
        "disk=none dseed=1 | run N=primary-replica@5-10:loss,100%",
        "disk=none dseed=1 | run N=a-*@5-10:loss,100%",
        "disk=none | run O=redis@5-10:resp:loss,100%",
        "disk=none | run F=app<mysql@9#15:orders:150000",
        "disk=none | run F=redis<workload@3#2:500",
        "disk=none | run F=redis>kafka@3:500",
        "disk=none | run F=a>b@4#0:1000",
        "disk=none | run H=orders.place@10-13:drop",
        "disk=none dseed=fbb60485ae17b0ba | run H=payments.charge@6-9:reset,p=50",
        "disk=none | run H=orders@0-30:status=UNAVAILABLE,effect=yes,rpc=Place,call=3-5",
        "disk=none | run H=orders@0-30:reset=conn,cut=2,dir=req,p=25,seed=417",
        "disk=none | run H=pay.hook@2-4:dup,reorder,late=5000,lost",
        "disk=none | run H=pay@2-4:status=503,retry-after=2,effect=no",
        "disk=none | run K=nkv@8+2 B=5abc",
        "disk=none | run B=5abc,1000,300",
        "disk=none dseed=1 | run W=0000002a,p4,q2,r100",
        "disk=none dseed=1d0b14e4db018fed | run W=fc94c8ea,p2,q2,r200,n2,x4,o0.4.6",
        "disk=ioerr:3 dseed=281cc34161f446f4 | run K=redis@1+1 W=6311ab23,p1,q2,r100,n6,x2-3,o0.6",
        "disk=none | run W=86f364de,p1,q1,r50,n7,x0.3-4,oanytime_read+parallel_driver_set",
        "disk=none dseed=aac8c00000a81e44 | run W=:p2,r100,cparallel_driver_transfer,2.1.0.605251887,3.1.0.644948928,21.10.0.1819369132",
        "disk=cut dseed=1 | run C=10 R=no W=:p1,r50,t300,cparallel_driver_set,1.1.0.733611656",
        "disk=none | run W=@77a712ab06ef",
        "disk=none | run W=-",
        "disk=none | run X=front@3~80 Q=4",
        "disk=cut dseed=0 cf=204 | run",
        "disk=cut dseed=0 cfn | run",
        "disk=cut dseed=0 q=50000 | run K=kafka@5+2 C=20",
        "disk=none dseed=0 tj=25ff9e5a67bfdc67 tjpm=200 tjmax=16383 pp=16384 | run S=32efe18d",
        "disk=none dseed=0 tj=7 tjpm=100 tjmax=500000 tjlo=12 tjhi=0 rb=16 | run",
        "disk=none dseed=0 tjx=3:100,9:2000 | run",
        "disk=none dseed=0 pp=65536 pptsc=4000000 | run",
        "disk=none dseed=0 xt=1f xtr=4 xtpm=500 xtlo=1 xthi=9 xtsc=7 | run",
        "disk=none dseed=0 xtx=100,2000,30000 | run",
        "disk=none dseed=0 q=100 qs=3f qb=2000:20:250 qbs=9 qm=160 | run",
        "disk=none dseed=0 pct=abc pctn=10 pctk=3000 spin=rewind | run",
        "disk=lying:12 dseed=ea0cf4c47365be7a | run C=5",
        "| run",
    ] {
        round_trip(s);
    }
}

#[test]
fn types() {
    let s = Schedule::parse(
        "disk=cut dseed=1f | run K=redis@3+2 C=12 R=everysec W=:p2,r100,cparallel_driver_x,0.1.0.5",
    )
    .unwrap();
    assert_eq!(s.disk(), Disk::Cut);
    assert_eq!(s.dseed(), 0x1f);
    assert!(s.has_power_cut());
    assert_eq!(s.host_text(), "disk=cut dseed=1f");
    assert_eq!(
        s.guest_text(),
        "run K=redis@3+2 C=12 R=everysec W=:p2,r100,cparallel_driver_x,0.1.0.5"
    );
    assert_eq!(
        s.guest[0],
        GuestToken::Kill {
            svc: "redis".into(),
            at: 3,
            down: 2
        }
    );
    assert_eq!(
        s.guest[2],
        GuestToken::Knob {
            letter: 'R',
            value: "everysec".into()
        }
    );
    match &s.guest[3] {
        GuestToken::Compose(Compose::Resolved(f)) => {
            assert_eq!(
                f[2],
                ResolvedField::Commands(vec!["parallel_driver_x".into()])
            );
            assert_eq!(
                f[3],
                ResolvedField::Entry {
                    entry: 0,
                    round: 1,
                    command: 0,
                    seed: 5
                }
            );
        }
        t => panic!("{t:?}"),
    }
    let c = Schedule::parse("disk=cut cf=3 | run").unwrap();
    assert!(c.has_power_cut());
    assert!(!Schedule::parse("disk=cut | run K=a@1+1")
        .unwrap()
        .has_power_cut());
    assert_eq!(Schedule::parse("| run").unwrap().disk(), Disk::None);
    let n = Schedule::parse("disk=none | run N=kafka@21-25:delay,20ms,reorder,11%").unwrap();
    assert_eq!(
        n.guest[0],
        GuestToken::Net {
            target: NetTarget::Service("kafka".into()),
            from: 21,
            to: 25,
            netem: vec![Netem::Delay(20), Netem::Reorder(11)]
        }
    );
}

/// The host part never reaches the guest, so a loose spelling is accepted and printed canonically
/// (the run is the same: the engines read the same numbers).
#[test]
fn host_side_normalises() {
    for (inp, out) in [
        ("disk=none  dseed=0x1F | run", "disk=none dseed=1f | run"),
        (
            "  dseed=001 disk=cut | run C=3",
            "dseed=1 disk=cut | run C=3",
        ),
        ("disk=ioerr:03 | run", "disk=ioerr:3 | run"),
        (
            "disk=none tj=00AB tjpm=010 tjmax=5 | run",
            "disk=none tj=ab tjpm=10 tjmax=5 | run",
        ),
        ("disk=none| run", "disk=none | run"),
        ("disk=none | run C=3  \n", "disk=none | run C=3"),
    ] {
        let p = Schedule::parse(inp).unwrap_or_else(|e| panic!("{inp:?}: {e}"));
        assert_eq!(p.to_string(), out, "{inp:?}");
        assert!(!Schedule::is_canonical(inp) || inp == out);
        assert!(Schedule::is_canonical(out));
    }
}

/// Refusals. Each says which side refuses the same string: "C" = hvx_explore.c parse_sched
/// ("bad schedule"), "py" = explore.py parse / FC_RE / H_RE (an exception), "guest" = the init
/// (a HYDO-ORACLE ERROR), "world" = hydo-world refuses the rule, "Rust" = refused only here
/// (stricter than the engines, which ignore or misread it).
#[test]
fn refusals() {
    // C: no '|'
    refused("disk=none run", "needs a '|'");
    // C: a disk value other than none, cut, ioerr:P, lying:P returns -1
    refused("disk=bogus | run", "the disk is none");
    refused("disk=lying | run", "the disk is none");
    // Rust (C: atoi("") = 0 is accepted as probability 0)
    refused("disk=ioerr: | run", "disk=ioerr:");
    refused("disk=lying:x | run", "disk=lying:");
    // C: cf=0, cf= without disk=cut (no disk= means none), cf= with a guest C=
    refused("disk=cut cf=0 | run", "cf=0");
    refused("disk=none cf=3 | run", "cf= needs disk=cut");
    refused("cf=3 | run", "cf= needs disk=cut");
    refused("disk=cut cf=3 | run C=5", "two power cuts");
    // C parse_turns: q=0, qs=0, malformed qb=, qb= with B or s 0 or permille > 1000, qbs= without qb=, qm= < 8
    refused("disk=none q=0 | run", "q=0");
    refused("disk=none qs=0 | run", "qs=0");
    refused("disk=none qb=5:2 | run", "expected <B>:<s>:<permille>");
    refused("disk=none qb=5:0:10 | run", "at least 1");
    refused("disk=none qb=5:2:1001 | run", "at most 1000");
    refused("disk=none qbs=3 | run", "needs qb=");
    refused("disk=none qm=7 | run", "at least 8");
    // C parse_tj / parse_preempt: more than 32 entries, an entry without ':' or not a number
    let many = (0..33)
        .map(|i| format!("{i}:1"))
        .collect::<Vec<_>>()
        .join(",");
    refused(&format!("disk=none tjx={many} | run"), "at most 32");
    refused("disk=none tjx=5 | run", "<tick>:<delay>");
    let many = (0..33).map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    refused(&format!("disk=none xtx={many} | run"), "at most 32");
    refused("disk=none xtx=a | run", "xtx= entry");
    // Rust: the C ignores these (an unknown word, a second copy, a modifier alone, both forms)
    refused("disk=none dsed=1 | run", "unknown host token");
    refused("disk=none disk=cut | run", "appears twice");
    refused("disk=none tjpm=5 | run", "without tj=");
    refused("disk=none tjlo=5 | run", "without tj=");
    refused("disk=none xtr=5 | run", "without xt=");
    refused("disk=none pctn=5 | run", "without pct=");
    refused("disk=none tj=1 tjx=1:2 | run", "together");
    refused("disk=none xt=1 xtx=1 | run", "together");
    refused("disk=none dseed=12zz | run", "not a hex number");
    refused("disk=none spin=always | run", "rewind");
    // Rust: the guest part is typed and logged as written, so its spacing is part of the run
    refused("disk=none |run", "one space after '|'");
    refused("disk=none |  run", "starts with \"run\"");
    refused("disk=none | run  C=3", "exactly one space");
    // Rust (the init only acts on a "run" command)
    refused("disk=none | go", "starts with \"run\"");
    refused("disk=none | runK=a@1+1", "starts with \"run\"");
    // py: explore.py parse splits every guest token at '=' (ValueError without one)
    refused("disk=none | run K", "expected <LETTER>=<value>");
    // Rust: the init reads only single upper-case letters and ignores the rest
    refused("disk=none | run k=1", "one upper-case letter");
    refused("disk=none | run KK=1", "one upper-case letter");
    refused("disk=none | run S=", "needs a value");
    // Rust: non-canonical guest numbers (the guest logs them as written; the shell would read
    // 05 as octal in $((..)))
    refused("disk=none | run K=redis@05+1", "leading zero");
    refused("disk=none | run C=+3", "not a decimal");
    refused("disk=none | run T=3:-0", "not -0");
    refused("disk=none | run B=0abc", "leading zero");
    refused("disk=none | run B=ABC", "lower-case hex");
    refused("disk=none | run K=Redis@1+1", "service name");
    refused("disk=none | run K=redis@1", "<svc>@<at>+<n>");
    refused("disk=none | run P=redis@1~5", "<svc>@<at>~<off>:<dur>");
    refused("disk=none | run X=front@1+5", "<svc>@<at>~<ms>");
    refused("disk=none | run Q=2x", "not a decimal");
    // Rust (explore.py's parse reads netem values with \D removed and tc gets the rest)
    refused("disk=none | run N=redis@1-2:loss", "in pairs");
    refused("disk=none | run N=redis@1-2:rate,1mbit", "netem kind");
    refused("disk=none | run N=redis@1-2:delay,1s", "<n>ms");
    refused("disk=none | run N=redis@1-2:loss,5", "percentage");
    // guest: hydo-init.sh die_oracle "bad one-way direction"
    refused("disk=none | run O=redis@1-2:both:loss,5%", "req or resp");
    // py: FC_RE (and the guest's fc_parse: no @, no > or <, no :<dur>, step/n/dur not numbers)
    refused("disk=none | run F=redis@3:500", "no > or <");
    refused("disk=none | run F=redis>kafka:500", "no @");
    refused("disk=none | run F=redis>kafka@3", "no :<dur>");
    refused("disk=none | run F=redis>kafka@x#1:500", "not a decimal");
    refused("disk=none | run F=Redis>kafka@3:500", "service name");
    // Rust: FC_RE takes an empty match, which explore.py then drops when it prints
    refused("disk=none | run F=redis>kafka@3#1::500", "match string");
    // py: H_RE (target lower case with at most one '.', a window, ':' and rules)
    refused("disk=none | run H=Pay@1-2:drop", "the target is");
    refused("disk=none | run H=a.b.c@1-2:drop", "the target is");
    refused("disk=none | run H=pay@1:drop", "the window is");
    refused("disk=none | run H=pay@1-2", "no :<rules>");
    refused("disk=none | run H=pay@1-2:", "no rules");
    // world: hydo-world refuses an unknown rule and these values when the fault is switched on
    refused("disk=none | run H=pay@1-2:explode", "unknown rule");
    refused("disk=none | run H=pay@1-2:effect=maybe", "effect=yes");
    refused("disk=none | run H=pay@1-2:reset=all", "reset=stream");
    refused("disk=none | run H=pay@1-2:dir=up", "dir=req");
    refused("disk=none | run H=pay@1-2:call=0", "counted from 1");
    refused("disk=none | run H=pay@1-2:call=5-3", "1 <= a <= b");
    refused("disk=none | run H=pay@1-2:delay=x", "not a decimal");
    // Rust: hydo-world accepts an empty rule (a stray ','), a value on drop and a lower-case name
    refused("disk=none | run H=pay@1-2:drop,", "unknown rule");
    refused("disk=none | run H=pay@1-2:drop=1", "takes no value");
    refused(
        "disk=none | run H=pay@1-2:status=unavailable",
        "upper-case gRPC status",
    );
    // py (hydo_composer.parse_w / parse_resolved) and Rust (the seed's spelling)
    refused("disk=none | run W=2a,p1", "8 lower-case hex digits");
    refused("disk=none | run W=0000002a,p1,z3", "unknown field");
    refused("disk=none | run W=0000002a,p1,", "unknown field");
    refused("disk=none | run W=0000002a,obogus_x", "command id");
    refused(
        "disk=none | run W=:r100,cparallel_driver_x,0.0.0.1",
        "has p, r and at least one entry",
    );
    refused(
        "disk=none | run W=:p1,r100,cparallel_driver_x",
        "has p, r and at least one entry",
    );
    refused(
        "disk=none | run W=:p1,r100,0.0.0.1,cparallel_driver_x",
        "of a table of 0",
    );
    refused(
        "disk=none | run W=:p1,r100,cparallel_driver_x,0.0.1.1",
        "of a table of 1",
    );
    refused(
        "disk=none | run W=:p1,r100,cparallel_driver_x,0.1.0.1,0.2.0.1",
        "unique",
    );
    refused(
        "disk=none | run W=:p1,r100,cparallel_driver_x,0.2.0.1,1.1.0.1",
        "rounds never go back",
    );
    refused(
        "disk=none | run W=:p1,r100,cnot_a_kind,0.0.0.1",
        "command id",
    );
    refused("disk=none | run W=@abc", "12 lower-case hex");
    // Rust: hvx_explore keeps 511 bytes of the guest part and would cut a longer one short
    let long = format!("disk=none | run {}", vec!["C=1"; 130].join(" "));
    refused(&long, "keeps 511");
}

/// M139: the reaction token `A=<event>[@<step>][+<ms>]:<action>`.
#[test]
fn reactions() {
    for s in [
        "disk=cut dseed=0 | run P=kafka@5+2 A=client.place_failed:cut",
        "disk=none dseed=0 | run O=kafka@5-10:resp:loss,100% A=client.outcome_unknown+20:kill-front",
        "disk=none dseed=0 | run P=kafka@5+25 A=client.outcome_unknown@5+500:cont-kafka",
        "disk=none dseed=0 | run A=server.store_error@3:stop-mysql-3000 A=client.place_failed:cut",
        "disk=none | run A=Fault-X.y_1:kill-redis",
    ] {
        round_trip(s);
    }
    let s = Schedule::parse("disk=none | run A=client.place_failed@4+250:stop-kafka-1000").unwrap();
    assert_eq!(
        s.guest,
        vec![GuestToken::React(React {
            event: "client.place_failed".into(),
            at: 4,
            delay_ms: 250,
            action: ReactAction::Stop("kafka".into(), 1000),
        })]
    );
    assert_eq!(s.guest[0].letter(), 'A');
    assert!(RESERVED.contains('A'));
    refused("disk=none | run A=x", "A=<event>");
    refused("disk=none | run A=x:boom", "the action is");
    refused("disk=none | run A=x:kill-", "not a service name");
    refused("disk=none | run A=x:stop-kafka", "the action is");
    refused("disk=none | run A=x:stop-kafka-0", "at least 1 ms");
    refused("disk=none | run A=x@0:cut", "no @0");
    refused("disk=none | run A=x+0:cut", "no +0");
    refused("disk=none | run A=x+05:cut", "leading zero");
    refused("disk=none | run A=:cut", "not an assertion id");
    refused("disk=none | run A=a/b:cut", "not an assertion id");
}

/// `<seed>`, `<outdir>` or an ellipsis: a pattern in the text, not a schedule.
fn placeholder(s: &str) -> bool {
    if s.contains('…') || s.contains("...") {
        return true;
    }
    s.match_indices('<').any(|(i, _)| {
        let rest = &s[i + 1..];
        rest.find('>')
            .is_some_and(|j| !rest[..j].contains(['@', '=', ' ', ':']))
    })
}

/// Every hand-written schedule in the documentation (`...| run ...` in backticks, or inside
/// `hydo run "..."`, also in table cells with `\|`) either parses and prints back unchanged, or is
/// a placeholder (a `<...>` or an ellipsis).
#[test]
fn documentation_schedules() {
    let top = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut files = vec![
        top.join("AGENTS.md"),
        top.join("README.md"),
        top.join("docs/HISTORY.md"),
        top.join("docs/REBASELINE.md"),
        top.join("hydo/onboard/MANIFEST.md"),
        top.join("hydo/dist/linux/QUICKSTART-linux.md"),
        top.join("hydo/dist/mac/QUICKSTART-mac.md"),
    ];
    for d in ["hydo/onboard/examples", "hydo/onboard/examples-mac"] {
        if let Ok(rd) = std::fs::read_dir(top.join(d)) {
            for e in rd.flatten() {
                files.push(e.path().join("RESULTS.md"));
                files.push(e.path().join("README.md"));
            }
        }
    }
    let (mut ok, mut placeholders) = (0, 0);
    let mut bad = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        for (i, chunk) in text.split('`').enumerate() {
            if i % 2 == 0 || !chunk.contains("| run") {
                continue;
            }
            // a Markdown line break inside the backticks is one space when rendered
            // ... and `\|` inside a table cell is a '|'
            let chunk = chunk
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .replace("\\|", "|");
            let chunk = chunk.as_str();
            let s = chunk
                .strip_prefix("hydo run \"")
                .or_else(|| chunk.strip_prefix("hydo repro \""))
                .and_then(|x| x.split('"').next())
                .unwrap_or(chunk);
            if !s.starts_with("disk=") && !s.starts_with("dseed=") {
                continue; // not a schedule (a command line that only mentions one)
            }
            if placeholder(s) {
                placeholders += 1;
                continue;
            }
            match Schedule::parse(s) {
                Ok(p) if p.to_string() == s => ok += 1,
                Ok(p) => bad.push(format!("{}: {s:?} prints as {p:?}", f.display())),
                Err(e) => bad.push(format!("{}: {s:?}: {e}", f.display())),
            }
        }
    }
    eprintln!("documentation: {ok} schedules round-trip, {placeholders} placeholders skipped");
    assert!(ok > 30, "{ok}");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
