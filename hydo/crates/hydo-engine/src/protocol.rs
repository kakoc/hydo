//! The engines' results and requests, typed (`hydo-engine-proto`, M142: the types moved there so
//! that the hvx engine, which runs in this process, takes and gives the same values). The text
//! form of each value is the line both engines print:
//!
//! ```text
//! READY boot_secs=<s>                                                       (serve, after S1)
//! RESULT <tag> fp=<hex16> wfp=<hex16> secs=<s>[ <key>=<value>...] verdict=<text>
//! BRANCHPOINT <tag> tjlo=<k> held=<n>                                       (hvx BRANCH only)
//! BRANCHEND <tag>[ <why>]                                                   (hvx BRANCH only)
//! ```
//!
//! The extras between `secs=` and `verdict=` are engine facts about the run: `tjk=<a>-<b> tjn=<n>`
//! (tick jitter: the workload's tick range and how many ticks were delayed), `xtn=` (extra ticks),
//! `qn=`/`qbn=` (vCPU turns), `pctf=`/`pctw=` (the Mac's forced preemption points).

pub use hydo_engine_proto::{
    verdict_kind, BranchOutcome, BranchRequest, EngineMsg, HostMsg, Ready, Request, RunResult,
};
