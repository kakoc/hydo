// HydoWork - the milestone-31 (Rung 5) real JVM workload run deterministically under hvx.
// =============================================================================
// A real, nontrivial Java program that deliberately exercises EVERY JVM
// nondeterminism source the ROADMAP names, so that hvx's two-boot full-state
// fingerprint (guest RAM + rw disk + console) is a genuine, honest test of JVM
// determinism -- if any source below is not tamed, the fingerprint diverges.
//
//   * heap allocation + GC        -- each worker churns millions of short-lived
//                                    long[] objects (garbage) while retaining a
//                                    bounded live set (promoted -> old gen), so
//                                    the SerialGC young + old collectors both run.
//   * many threads on ONE vCPU    -- N worker threads PLUS the JVM's own threads
//                                    (VM, GC, reference handler, finalizer, signal
//                                    dispatcher). They contend on a shared, ORDER-
//                                    DEPENDENT accumulator (state = state*M + x under
//                                    a lock), so the final `state` is a fingerprint
//                                    of the EXACT interleaving: if thread scheduling
//                                    on the single deterministic vCPU were not
//                                    reproducible, `state` would differ across boots.
//   * System.nanoTime / currentTimeMillis -> the deterministic virtual TSC/clock.
//   * SecureRandom (default: reads /dev/urandom) -> the M27 CRNG seed.
//   * java.util.Random (fixed seed)   -> deterministic by contract (a control).
//   * System.identityHashCode(obj)    -> the address/seed-dependent source.
//
// The observed values of all of these are PRINTED to stdout (-> the hvx console,
// which is part of the fingerprint) so their determinism is proven directly, and a
// mismatch is caught. The 32 committed key records written to /data are the M29-style
// durability-fault target (a seeded disk DROP loses one -> oracle VIOLATION).
//
// Compiled with a pinned javac 17 to a single .class (no embedded timestamp ->
// byte-reproducible) and staged onto the ext4 ro-root; the JRE runs it with the
// determinism flags (-Xint -XX:+UseSerialGC -XX:-UsePerfData fixed heap ...).

import java.io.FileWriter;
import java.io.IOException;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.Random;

public final class HydoWork {

    static final int NKEYS    = 32;         // committed records -> the rw data disk
    static final int NTHREADS = 4;          // worker threads (plus the JVM's own)
    static final int ITERS    = 500_000;    // per-thread alloc loop: 4*500k*~80B = ~160 MB
                                            // churn in a 64 MB heap -> GC MUST run to finish
    static final long MIX_M    = 1000003L;  // order-dependent mixing multiplier

    // Shared, LOCK-PROTECTED, ORDER-DEPENDENT accumulator. Because the update is
    // state = state*M + x (NOT commutative), the final value depends on the exact
    // order in which threads acquire the lock -- i.e. on the vCPU scheduling. Two
    // boots must reach the identical `state` for the fingerprint to match.
    private static long state = 0xC0FFEE123L;
    private static final Object lock = new Object();

    private static void mix(long x) {
        synchronized (lock) {
            state = state * MIX_M + x;
        }
    }

    public static void main(String[] args) throws Exception {
        String out = (args.length > 0) ? args[0] : "/data";

        // ---- 1. multi-threaded allocation + GC + order-dependent contention ----
        final long[] partial = new long[NTHREADS];
        Thread[] ts = new Thread[NTHREADS];
        for (int t = 0; t < NTHREADS; t++) {
            final int id = t;
            ts[t] = new Thread(() -> {
                long acc = 0;
                Random r = new Random(0x9E3779B97F4A7C15L + id); // fixed seed: deterministic
                ArrayList<long[]> live = new ArrayList<>();       // bounded live set -> old gen
                for (int i = 0; i < ITERS; i++) {
                    long[] a = new long[8];                       // garbage: churns the heap
                    a[0] = r.nextLong();
                    a[1] = (long) i * 31 + id;
                    acc += a[0] ^ a[1];
                    if ((i & 255) == 0) {                          // retain some -> promote
                        live.add(a);
                        if (live.size() > 512) live.remove(0);     // and drop old -> GC churn
                    }
                    if ((i & 63) == 0) mix(acc + id);              // contend on the shared lock
                }
                long h = 0;
                for (long[] a : live) h ^= a[0];
                partial[id] = acc + h + live.size();
            }, "hydo-worker-" + t);
        }
        for (Thread th : ts) th.start();
        for (Thread th : ts) th.join();

        long sum = 0;
        for (long p : partial) sum += p;
        long finalState;
        synchronized (lock) { finalState = state; }

        // ---- 2. exercise the clock (-> deterministic virtual TSC) ----
        long nano0 = System.nanoTime();
        long millis = System.currentTimeMillis();
        long nano1 = System.nanoTime();
        boolean nanoMonotonic = (nano1 >= nano0);

        // ---- 3. exercise SecureRandom (default provider reads /dev/urandom, seeded
        //         by the M27 CRNG) + java.util.Random control + identity hash codes ----
        SecureRandom sr = new SecureRandom();
        byte[] rb = new byte[16];
        sr.nextBytes(rb);
        Random control = new Random(42);
        long controlVal = control.nextLong();
        Object o1 = new Object();
        Object o2 = new Object();
        int id1 = System.identityHashCode(o1);
        int id2 = System.identityHashCode(o2);

        // ---- 4. write NKEYS committed records to the rw DATA disk (the durability
        //         fault target; value a pure function of the key, exactly like M29) ----
        for (int k = 0; k < NKEYS; k++) {
            writeFile(out + "/key" + k, Integer.toString(100000 + k));
        }

        // ---- 5. print every observed value to the console (part of the fingerprint):
        //         if ANY of these is non-deterministic, the two-boot fingerprint
        //         diverges and hvx catches it. ----
        StringBuilder sb = new StringBuilder();
        sb.append("HYDO-JVM: threads=").append(NTHREADS)
          .append(" iters=").append(ITERS)
          .append(" sum=").append(sum)
          .append(" state=").append(finalState)
          .append(" secrand=").append(hex(rb))
          .append(" control=").append(controlVal)
          .append(" idhash=").append(id1).append(",").append(id2)
          .append(" nanoMono=").append(nanoMonotonic)
          .append(" millisNZ=").append(millis != 0);
        System.out.println(sb.toString());
        System.out.println("HYDO-JVM: gc.total=" + gcCount() + " done");
    }

    private static long gcCount() {
        long n = 0;
        try {
            for (java.lang.management.GarbageCollectorMXBean b :
                     java.lang.management.ManagementFactory.getGarbageCollectorMXBeans()) {
                long c = b.getCollectionCount();
                if (c > 0) n += c;
            }
        } catch (Throwable ignore) { /* management beans optional */ }
        return n;
    }

    private static String hex(byte[] b) {
        char[] H = "0123456789abcdef".toCharArray();
        StringBuilder s = new StringBuilder(b.length * 2);
        for (byte x : b) {
            s.append(H[(x >> 4) & 0xf]);
            s.append(H[x & 0xf]);
        }
        return s.toString();
    }

    private static void writeFile(String path, String data) throws IOException {
        try (FileWriter w = new FileWriter(path)) {
            w.write(data);
        }
    }
}
