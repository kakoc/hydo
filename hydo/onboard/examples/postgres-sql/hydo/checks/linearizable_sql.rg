#!/bin/sh
# hydo: history=rg model=cas
# hydo-msg: the SQL registers are linearizable (no lost write, no stale read on a primary)
# A replica's reads are not recorded (they may be stale); its writes are.
# The final read-back: every register on db.
exec hydo-sql register read-all db 9997
