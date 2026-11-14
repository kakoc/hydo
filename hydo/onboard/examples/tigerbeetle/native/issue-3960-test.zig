// The unit test of TigerBeetle's issue 3960, copied from the issue
// (https://github.com/tigerbeetle/tigerbeetle/issues/3960). unit-test.sh appends it to
// src/state_machine_tests.zig, whose `check` runs the table.

test "expire_pending_transfers: expiry not before expires_at" {
    try check(
        \\ account A1  0  0  0  0  _  _  _ _ L1 C1   _   _   _ _ _ _ _ _ created
        \\ account A2  0  0  0  0  _  _  _ _ L1 C1   _   _   _ _ _ _ _ _ created
        \\ commit create_accounts
        \\ transfer   T1 A1 A2   10   _  _  _  _    2 L1 C1   _ PEN   _   _   _   _  _ _ _ _ _ created
        \\ commit create_transfers
        \\ tick 29 nanoseconds
        \\ transfer   T2 A1 A2   10   _  _  _  _    2 L1 C1   _ PEN   _   _   _   _  _ _ _ _ _ created
        \\ commit create_transfers
        \\ tick 999999999 nanoseconds
        \\ transfer   T3 A1 A2   10   _  _  _  _    1 L1 C1   _ PEN   _   _   _   _  _ _ _ _ _ created
        \\ commit create_transfers
        \\ tick 999999970 nanoseconds
        \\ tick 1 seconds
        \\ lookup_account A1   0  0   0  0  _
        \\ lookup_account A2   0  0   0  0  _
        \\ commit lookup_accounts
    );
}
