//! Hybrid CPUs (performance and efficiency cores of different designs, Intel since Alder Lake):
//! an x86-64-v3 guest sees which kind of core it runs on. The guest's own XSAVE writes the
//! header's XSTATE_BV, and whether the processor tracks a component in its initial state
//! ("XINUSE", the init optimization) is the core design's choice: on the build machine's Core
//! i7-13700HX the x87 bit of a saved area is 0 on a performance core and 1 on an efficiency core
//! for the same guest. A vCPU thread that moved between the two kinds during a run gave a third
//! fingerprint. So a v3 vCPU runs on one kind only ([`choose`]), the performance cores when the
//! process may use any of them; a v2 guest has no XSAVE and runs anywhere, as before
//! (docs/design/kvm-engine.md, "x86-64-v3").

/// The CPUs of a Linux CPU list (`/sys/devices/cpu_core/cpus`: `0-15`, `0,2,4-7`).
pub fn cpulist(s: &str) -> Vec<usize> {
    let mut v = Vec::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim().parse::<usize>(), b.trim().parse::<usize>()),
            None => (part.trim().parse::<usize>(), part.trim().parse::<usize>()),
        };
        if let (Ok(a), Ok(b)) = (a, b) {
            v.extend(a..=b);
        }
    }
    v
}

/// The CPUs a v3 vCPU thread may run on: of the CPUs it may use now (`allowed`), the
/// performance cores (`core`), or the efficiency cores (`atom`) when it may use no performance
/// core. None: not a hybrid CPU (one of the lists is empty), or none of `allowed` is listed.
pub fn choose(allowed: &[usize], core: &[usize], atom: &[usize]) -> Option<Vec<usize>> {
    if core.is_empty() || atom.is_empty() {
        return None;
    }
    let of = |kind: &[usize]| -> Vec<usize> {
        allowed
            .iter()
            .copied()
            .filter(|c| kind.contains(c))
            .collect()
    };
    let p = of(core);
    if !p.is_empty() {
        return Some(p);
    }
    let e = of(atom);
    (!e.is_empty()).then_some(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists() {
        assert_eq!(cpulist("0-3\n"), vec![0, 1, 2, 3]);
        assert_eq!(cpulist("0,2,4-6"), vec![0, 2, 4, 5, 6]);
        assert_eq!(cpulist(""), Vec::<usize>::new());
    }

    #[test]
    fn one_kind_of_core() {
        let core: Vec<usize> = (0..16).collect();
        let atom: Vec<usize> = (16..24).collect();
        let all: Vec<usize> = (0..24).collect();
        assert_eq!(choose(&all, &core, &atom), Some(core.clone()));
        assert_eq!(choose(&[3, 17], &core, &atom), Some(vec![3]));
        assert_eq!(choose(&[16, 20], &core, &atom), Some(vec![16, 20]));
        assert_eq!(choose(&all, &[], &[]), None, "not hybrid");
        assert_eq!(choose(&all, &core, &[]), None);
        assert_eq!(choose(&[30], &core, &atom), None);
    }
}
