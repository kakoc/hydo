//! The schedule in plain words (`describe.rs`, Milestone 220): every token of the language has a
//! sentence and a gloss, the words never name hydo's own machinery, and the sentences come in the
//! order a reader follows a run.

use hydo_schedule::describe::{self, KnobInfo, Part, Said};
use hydo_schedule::Schedule;

/// Words of hydo's own machinery that a reader of the plain words should never meet.
const JARGON: [&str; 11] = [
    "fingerprint",
    "epoch",
    "engine",
    "schedule",
    "token",
    "invariant",
    "oracle",
    "signature",
    "class",
    "seed",
    "netem",
];

fn plain(s: &Said) {
    let mut texts = vec![&s.sentence, &s.gloss, &s.noun];
    if let Some(c) = &s.context {
        texts.push(c);
    }
    for text in texts {
        assert!(!text.trim().is_empty(), "{}: empty words", s.token);
        let low = text.to_lowercase();
        for w in low.split(|c: char| !c.is_ascii_alphanumeric()) {
            for j in JARGON {
                assert!(
                    w != j && w != format!("{j}s"),
                    "{}: {text:?} uses the word {w:?}",
                    s.token
                );
            }
        }
        assert!(
            !text.contains('\u{2014}'),
            "{}: a long dash in {text:?}",
            s.token
        );
        assert!(!low.contains("knob"), "{}: {text:?}", s.token);
    }
    assert!(
        s.sentence.ends_with('.'),
        "{}: {:?} is not a sentence",
        s.token,
        s.sentence
    );
    assert!(
        s.sentence
            .starts_with(|c: char| c.is_uppercase() || c.is_ascii_digit() || c == 'h'),
        "{}: {:?} does not start a sentence",
        s.token,
        s.sentence
    );
}

fn redis_knob() -> Vec<KnobInfo> {
    vec![KnobInfo {
        letter: 'R',
        name: Some("Redis appendfsync".into()),
        default: Some("always".into()),
        safe: None,
        description: None,
    }]
}

fn one(text: &str) -> Said {
    let all = describe::describe_text(text, &redis_knob());
    assert!(!all.is_empty());
    all.into_iter().next().unwrap()
}

/// Every host token, every guest token and every form of each: a sentence and a gloss in plain
/// words.
#[test]
fn every_token_has_plain_words() {
    let host = [
        "disk=none",
        "disk=cut",
        "disk=ioerr:5",
        "disk=lying:3",
        "dseed=1f16055b6d41c6d9",
        "tj=7",
        "tjpm=100",
        "tjmax=500000",
        "tjlo=5",
        "tjhi=0",
        "tjhi=40",
        "tjx=8:83779,9:412915",
        "pp=65536",
        "pptsc=1000",
        "xt=66882e0bfabc71b3",
        "xtr=4",
        "xtpm=500",
        "xtlo=1",
        "xthi=9",
        "xtsc=100",
        "xtx=100,2000,30000",
        "cf=3",
        "cfn",
        "rb=64",
        "q=890681",
        "qs=3ef5afb8de981432",
        "qb=99561:920:500",
        "qbs=3c72a63e5cabce79",
        "qm=8",
        "pct=5f99cf09e98ec801",
        "pctn=3",
        "pctk=100",
        "spin=rewind",
        "br=900",
    ];
    let mut seen_host = std::collections::BTreeSet::new();
    for h in host {
        let t = hydo_schedule::HostToken::parse(h).unwrap();
        seen_host.insert(t.key());
        let s = describe::host(&t, &Schedule::parse("disk=ioerr:1 | run").unwrap());
        assert_eq!(s.token, h);
        plain(&s);
    }
    // every key of the host part (a new token needs words here)
    assert_eq!(seen_host.len(), 30, "host keys seen: {seen_host:?}");
    let guest = [
        "K=redis@5+2",
        "K=n1,n2@5+2",
        "K=nodes.one@5+2",
        "K=nodes.minority@5+2",
        "K=nodes.majority.1@5+2",
        "K=nodes.all@5+2",
        "K=nodes.primary@5+1",
        "P=redis@3+2",
        "P=redis@5~0:100",
        "P=workload@0~180:60",
        "P=workload,redis@5~0:1",
        "P=workload/tokio-rt-worker#2@0~100:200",
        "P=workload/tests::wal_reco@0~4433:500",
        "N=redis@7-13:duplicate,36%,loss,57%",
        "N=redis@7-13:delay,263ms",
        "N=a-b@3-9:reorder,10%,corrupt,5%",
        "N=a-*@3-9:loss,100%",
        "N=n1/n2,n3@3-9:loss,100%",
        "N=nodes.halves@3-9:loss,100%",
        "N=nodes.isolate.2@3-9:loss,100%",
        "N=nodes.ring@3-9:loss,100%",
        "N=nodes.bridge@3-9:loss,100%",
        "N=nodes.primary@3-9:loss,100%",
        "O=kafka@28-30:resp:loss,67%",
        "O=kafka@24-25:req:delay,335ms",
        "C=0",
        "C=1",
        "C=12",
        "T=25:-300",
        "T=3:3600",
        "T=3:31536000",
        "T=0:@:10-2s",
        "T=0:@00:10",
        "T=0:@12:30:15",
        "T=0:@hour",
        "T=0:@day",
        "T=0:@month+10m",
        "T=0:@year",
        "Q=8",
        "Q=4d",
        "X=redis@5~300",
        "B=b374246f0193c21",
        "B=1,500,20",
        "H=redisp@14-15:delay=10000,p=25",
        "H=payments.charge@6-9:reset,p=50",
        "H=mailgun.messages@0-12:status=500,effect=yes",
        "H=api@1-2:status=UNAVAILABLE,rpc=Get,call=2-4,dir=resp,hold=3",
        "H=api@1-2:status=429,retry-after=5,seed=x",
        "H=api@1-2:drop,call=3",
        "H=hooks@1-9:late=500,dup,reorder,lost,effect=no",
        "H=api@1-2:reset=stream,cut=2,dir=req",
        "H=api@1-2:reset=conn",
        "F=replica>primary@4#57:3000",
        "F=redis<workload@2:SET:500",
        "F=redis>workload@2#0:500",
        "W=-",
        "W=041bde7a,p1,q1,r100,n12",
        "W=@8470df6b239a",
        "W=:p3,r100,cparallel_driver_set+anytime_read,0.0.0.1979818538,2.0.1.1550708684",
        "A=client.place_failed:cut",
        "A=client.outcome_unknown+20:kill-front",
        "A=ev@3:stop-db-200",
        "A=ev:cont-db",
        "G=redis@5+2:flip,n=3,file=aof",
        "G=redis@5:zero,len=512,range=0-4096",
        "G=redis@5:trunc",
        "G=redis@5:trunc,len=10,seed=2",
        "L=redis@3+2",
        "R=everysec",
        "R=always",
        "M=2",
    ];
    let mut letters = std::collections::BTreeSet::new();
    for g in guest {
        let t = hydo_schedule::GuestToken::parse(g).unwrap_or_else(|e| panic!("{g}: {e}"));
        letters.insert(t.letter());
        let s = describe::guest(&t, &redis_knob());
        assert_eq!(s.token, g);
        plain(&s);
    }
    // every letter with a fixed meaning, and the settings of the manifest
    for c in hydo_schedule::RESERVED.chars() {
        assert!(letters.contains(&c), "no example for {c}=");
    }
}

#[test]
fn a_power_cut_with_a_relaxed_setting() {
    let all = describe::describe_text("disk=cut | run C=12 R=everysec", &redis_knob());
    let lines: Vec<(&str, Part)> = all.iter().map(|s| (s.sentence.as_str(), s.part)).collect();
    assert_eq!(
        lines,
        vec![
            (
                "The disk keeps recent writes in a cache, as real disks do: a power cut loses the \
                 writes that the system had not yet flushed to disk (fsync).",
                Part::Setup
            ),
            (
                "Redis appendfsync is set to `everysec` instead of `always`, its safe value.",
                Part::Setup
            ),
            (
                "The power goes out after 12 steps of your workload, before the 13th step. Then \
                 the machine starts again.",
                Part::Fault
            ),
        ]
    );
    let glosses: Vec<(&str, &str)> = all
        .iter()
        .map(|s| (s.token.as_str(), s.gloss.as_str()))
        .collect();
    assert_eq!(
        glosses,
        vec![
            (
                "disk=cut",
                "a power cut loses the writes not yet flushed to disk"
            ),
            ("R=everysec", "Redis appendfsync set to `everysec`"),
            ("C=12", "cut the power after 12 steps"),
        ]
    );
    assert_eq!(all[2].context.as_deref(), Some("after a power cut"));
    assert_eq!(all[2].step, Some(12));
}

#[test]
fn faults_come_in_the_order_of_their_steps() {
    let all = describe::describe_text(
        "disk=ioerr:1 dseed=910a2dec89025cc1 rb=16 | run K=redis@12+2 N=redis@7-13:delay,10ms T=3:-300",
        &[],
    );
    let toks: Vec<&str> = all.iter().map(|s| s.token.as_str()).collect();
    assert_eq!(
        toks,
        vec![
            "disk=ioerr:1",
            "T=3:-300",
            "N=redis@7-13:delay,10ms",
            "K=redis@12+2",
            "dseed=910a2dec89025cc1",
            "rb=16",
        ]
    );
    assert_eq!(
        all[1].sentence,
        "After 3 steps, the system clock jumps back by 5 minutes."
    );
    assert_eq!(
        all[3].sentence,
        "hydo kills `redis` after 12 steps, as in a crash, and starts it again after 14 steps."
    );
    assert_eq!(all[4].part, Part::Quiet);
}

#[test]
fn a_setting_without_its_manifest_entry_and_a_described_one() {
    let s = one("| run S=fast");
    assert_eq!(s.sentence, "Your project's setting `S` is set to `fast`.");
    let k = vec![KnobInfo {
        letter: 'S',
        name: Some("sync mode".into()),
        default: Some("full".into()),
        safe: Some("full".into()),
        description: Some("how often the WAL is synced".into()),
    }];
    let s = describe::describe_text("| run S=fast", &k).remove(0);
    assert_eq!(
        s.sentence,
        "Sync mode is set to `fast` instead of `full`, its safe value. How often the WAL is synced."
    );
    let s = describe::describe_text("| run S=full", &k).remove(0);
    assert_eq!(
        s.sentence,
        "Sync mode is set to `full`. How often the WAL is synced."
    );
}

#[test]
fn a_word_that_is_not_a_token_gets_a_neutral_sentence() {
    let all = describe::describe_text("disk=cut zz=1 | run C=3 kk", &[]);
    let toks: Vec<&str> = all.iter().map(|s| s.token.as_str()).collect();
    assert_eq!(toks, vec!["disk=cut", "zz=1", "kk", "C=3"]);
    for s in &all {
        plain(s);
    }
    assert!(all[1].sentence.contains("`zz=1`"));
    assert_eq!(
        all[1].sentence,
        "The run also has the setting `zz=1`. `hydo guide faults` lists what each setting does."
    );
}

#[test]
fn how_one_run_differs_from_another() {
    let k = redis_knob();
    let a = "disk=cut | run C=12 R=everysec";
    assert_eq!(
        describe::change(a, "disk=cut | run C=12 R=always", &k).as_deref(),
        Some("with Redis appendfsync at `always` instead of `everysec`")
    );
    assert_eq!(
        describe::change(a, "disk=cut | run R=everysec", &k).as_deref(),
        Some("without a power cut after 12 steps")
    );
    assert_eq!(describe::change(a, a, &k), None);
    assert_eq!(
        describe::change(
            "disk=lying:1 dseed=63cbe1e459320dd7 | run C=11",
            "disk=none dseed=63cbe1e459320dd7 | run C=11",
            &k
        )
        .as_deref(),
        Some("with a disk that keeps every write at once instead of a faulty disk that throws away writes it already confirmed")
    );
    // a change of one token and a removed one
    assert_eq!(
        describe::change(
            "disk=cut dseed=1f16055b6d41c6d9 | run C=9 R=no",
            "disk=none dseed=1f16055b6d41c6d9 | run R=no",
            &k
        )
        .as_deref(),
        Some("with a disk that keeps every write at once instead of a disk whose cache a power cut loses, and without a power cut after 9 steps")
    );
    // the same disk fault at another rate
    assert_eq!(
        describe::change(
            "disk=lying:3 dseed=eac0b527c5ac67d9 | run C=20",
            "disk=lying:1 dseed=eac0b527c5ac67d9 | run C=20",
            &k
        )
        .as_deref(),
        Some("with the disk throwing away fewer writes")
    );
    assert_eq!(
        describe::change("disk=none | run", "disk=none | run K=redis@5+2", &k).as_deref(),
        Some("with a crash of `redis` after 5 steps added")
    );
}

#[test]
fn durations_and_clock_points() {
    assert_eq!(describe::duration(30), "30 seconds");
    assert_eq!(describe::duration(300), "5 minutes");
    assert_eq!(describe::duration(3600), "1 hour");
    assert_eq!(describe::duration(86_400), "1 day");
    assert_eq!(describe::duration(2_592_000), "30 days");
    assert_eq!(describe::duration(1), "1 second");
    assert_eq!(
        one("| run T=0:@:10-2s").sentence,
        "Before the first step, the system clock jumps forward to 2 seconds before the next \
         minute 10 of an hour."
    );
    assert_eq!(
        one("| run T=4:@month+10m").sentence,
        "After 4 steps, the system clock jumps forward to 10 minutes after the first day of the \
         next month, 00:00 UTC."
    );
}

#[test]
fn faults_on_groups_and_outside_services() {
    assert_eq!(
        one("| run K=nodes.majority@5+2").sentence,
        "hydo kills a majority of the group `nodes` after 5 steps, as in a crash, and starts \
         them again after 7 steps."
    );
    assert_eq!(
        one("| run N=nodes.halves@3-9:loss,100%").sentence,
        "During steps 4 to 9, hydo cuts every link between two halves of the group `nodes`: the \
         two sides cannot reach each other."
    );
    assert_eq!(
        one("| run H=payments.charge@6-9:reset,p=50").sentence,
        "During steps 7 to 9, the outside service `payments`, on its route `charge`, resets the \
         connection for 50% of the requests."
    );
    assert_eq!(
        one("| run P=workload/tokio-rt-worker#2@0~100:200").sentence,
        "100 ms after the first step starts, hydo pauses the 2nd thread named `tokio-rt-worker` of \
         your workload for 200 ms."
    );
    assert_eq!(
        one("| run O=kafka@28-30:resp:loss,67%").sentence,
        "During steps 29 to 30, the network drops 67% of the packets on the answers from \
         `kafka`; the requests pass normally."
    );
}
