#!/usr/bin/env bash
# Assemble every guest in guests/*.s into a flat binary blob in build/.
#
# We link with an explicit script that (a) discards ELF notes/eh_frame so they
# do not perturb addresses, and (b) places one contiguous section at the guest's
# load address. That guarantees symbol addresses equal file-offset + load-addr,
# so absolute references in the guest resolve correctly once loaded.
set -euo pipefail

mkdir -p build

link_addr() {
  local f="$1" a
  a=$(sed -n '1s/^#[[:space:]]*load:[[:space:]]*//p' "$f")
  echo "${a:-0x1000}"
}

for s in guests/*.s; do
  [ -e "$s" ] || continue
  name=$(basename "$s" .s)
  addr=$(link_addr "$s")

  cat > "build/$name.ld" <<LDS
ENTRY(_start)
SECTIONS {
  . = ${addr};
  .all : {
    *(.text*)
    *(.rodata*)
    *(.data*)
    *(.bss*)
  }
  /DISCARD/ : { *(.note*) *(.eh_frame*) *(.comment) *(.gnu*) }
}
LDS

  as --64 -o "build/$name.o" "$s"
  ld -T "build/$name.ld" -o "build/$name.elf" "build/$name.o"
  objcopy -O binary "build/$name.elf" "build/$name.bin"
  printf "  built %-16s load=%s size=%s bytes\n" "$name.bin" "$addr" "$(stat -c%s build/$name.bin)"
done
