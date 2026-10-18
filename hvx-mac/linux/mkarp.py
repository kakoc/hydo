#!/usr/bin/env python3
"""Write scripted inbound frames for hvm's HVM_NET_RX: [u16 big-endian length][frame] each.
Default: three ARP requests "who-has 10.0.0.2 tell 10.0.0.1" (paced, so a later copy arrives
after the guest's interface is up). Usage: mkarp.py <out> [copies]"""
import struct, sys
src_mac = bytes.fromhex("525400abcdef"); src_ip = bytes([10, 0, 0, 1]); dst_ip = bytes([10, 0, 0, 2])
arp = struct.pack("!HHBBH", 1, 0x0800, 6, 4, 1) + src_mac + src_ip + b"\0" * 6 + dst_ip
frame = b"\xff" * 6 + src_mac + b"\x08\x06" + arp
frame += b"\0" * (60 - len(frame))
with open(sys.argv[1], "wb") as f:
    for _ in range(int(sys.argv[2]) if len(sys.argv) > 2 else 3):
        f.write(struct.pack("!H", len(frame)) + frame)
