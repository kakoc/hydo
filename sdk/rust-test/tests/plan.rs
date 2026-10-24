use hydo_test::prelude::*;

#[driver(parallel, timeout = 7)]
fn place(ctx: &Ctx) -> Result<()> {
    if ctx.seed() == 1 { Err(violation!("x")) } else { Ok(()) }
}

#[driver(first)]
fn seed_data(_ctx: &Ctx) -> Result<()> {
    Ok(())
}

#[check(final, msg = "a \"quoted\" message")]
fn holds(_ctx: &Ctx) -> Result<()> {
    Ok(())
}

#[check(eventually, within = 9, after = "start")]
fn settles(_ctx: &Ctx) -> Result<()> {
    Err(fail!("not yet"))
}

#[test]
fn plan_lists_every_function_in_a_fixed_order() {
    let regs = hydo_test::registrations();
    let names: Vec<String> = regs.iter().map(|r| format!("{} {} {}", r.role, r.kind, r.name)).collect();
    assert_eq!(names, ["check eventually settles", "check final holds", "driver first seed_data", "driver parallel_driver place"]);
    let sys = System::new("t").service(Service::new("db", "/bin/db").ready("true").port(5432)).steps(8)
        .faults(&[Fault::Kill, Fault::Cut]).regression("r", "disk=none dseed=1 | run", Expect::Ok);
    let j = sys.plan_json("/tmp/g", "/bin/t", &regs);
    for want in ["\"format\": 1", "\"hydo_version\": \"0.1\"", "\"kind\": \"parallel_driver\"", "\"timeout\": 7",
                 "\"run\": [\n", "\"/bin/t\"", "\"driver\"", "\"place\"", "\"within\": 9", "\"after\": \"start\"",
                 "a \\\"quoted\\\" message", "\"faults\": [\n", "\"expect\": \"ok\""] {
        assert!(j.contains(want), "missing {:?} in\n{}", want, j);
    }
    assert_eq!(j, sys.plan_json("/tmp/g", "/bin/t", &regs), "the plan is a pure function");
}

#[test]
fn history_checks_become_test_checks() {
    let regs = hydo_test::registrations();
    let sys = System::new("t").service(Service::new("db", "/bin/db").ready("true")).steps(8)
        .history_check(HistoryCheck::linearizable("db.kv_linearizable", "kv").model("cas").readback(&["/bin/t", "readback"]))
        .history_check(HistoryCheck::list_append("db.log", "log").consistency("serializable"));
    let j = sys.plan_json("/tmp/g", "/bin/t", &regs);
    for want in ["\"kind\": \"linearizable\"", "\"history\": \"kv\"", "\"model\": \"cas\"", "\"readback\"",
                 "\"kind\": \"list_append\"", "\"consistency\": \"serializable\""] {
        assert!(j.contains(want), "missing {:?} in\n{}", want, j);
    }
    if let Ok(p) = std::env::var("HYDO_PLAN_OUT") {      // for a hydo-build --check by hand
        std::fs::write(p, &j).unwrap();
    }
}
