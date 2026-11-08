# Build the hvx VT-x module against the running kernel's headers (KDIR). Load it on an Intel
# host with KVM unloaded, under the module lock (CONTRIBUTING.md, "Build the engines").
obj-m := hvx.o

# hvx.c contains hand-written asm functions (hvx_vmx_run, hvx_guest_code) whose
# bare `ret` and manual push/pop trip objtool under RETHUNK/ORC. Skip objtool for
# this object; the code is correct by construction.
OBJECT_FILES_NON_STANDARD_hvx.o := y

KDIR ?= /lib/modules/$(shell uname -r)/build
PWD  := $(shell pwd)

all:
	$(MAKE) -C $(KDIR) M=$(PWD) modules

clean:
	$(MAKE) -C $(KDIR) M=$(PWD) clean
