import os,struct,sys
def rd(c,m):
    f=os.open("/dev/cpu/%d/msr"%c,os.O_RDONLY); v=struct.unpack("<Q",os.pread(f,8,m))[0]; os.close(f); return v
n=os.cpu_count()
print(" ".join(str(rd(c,0x34)) for c in range(n)), "perfcap=%x"%rd(0,0x345), "debugctl=%x"%rd(0,0x1d9))
