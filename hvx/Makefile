# Build the hvx VT-x module against the running L1 kernel.
# Run this INSIDE the L1 sandbox guest, never on the host.
obj-m := hvx.o

# hvx.c contains hand-written asm functions (hvx_vmx_run, hvx_guest_code) whose
# bare `ret` and manual push/pop trip objtool under RETHUNK/ORC. Skip objtool for
# this object; the code is correct by construction and runs only inside L1.
OBJECT_FILES_NON_STANDARD_hvx.o := y

KDIR ?= /lib/modules/$(shell uname -r)/build
PWD  := $(shell pwd)

all:
	$(MAKE) -C $(KDIR) M=$(PWD) modules

clean:
	$(MAKE) -C $(KDIR) M=$(PWD) clean
