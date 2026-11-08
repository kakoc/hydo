# Licensing of the Linux engine

hydo as a whole is licensed under the Apache License 2.0 (`LICENSE` at the repository root). The
Linux kernel module is the exception:

- `hvx.c`, the kernel module (`hvx.ko`, built by `Makefile`, and the side modules built from the
  same source by `avx/`, `fastturn/` and `safety/`), is licensed under the **GNU General Public
  License, version 2 only** (`LICENSE-GPL-2.0` in this folder). A Linux kernel module must be
  GPL-compatible to use the kernel's GPL-only symbols, and the module declares
  `MODULE_LICENSE("GPL")`.
- `hvx_abi.h`, the module's ioctl interface, is included both by the module and by user-space
  programs. It is licensed under **GPL-2.0-only WITH Linux-syscall-note OR Apache-2.0**, the same
  arrangement Linux uses for its own user-space interface headers, so that Apache-2.0 user-space
  code may include it.
- Everything else in this folder (the tests and harnesses, the guest-image scripts and the engine
  tools) is Apache-2.0, like the rest of the repository.

The GPL-2.0 covers the module's source only. It does not reach programs that use the module
through `/dev/hvx`, nor the software tested under hydo.
