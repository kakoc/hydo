#!/bin/sh
# run each spin variant at several kick timings; print fp + resolve stats
cd "$(dirname "$0")/.."
for v in spin load ldar yield samestore ldxr count; do
  for ms in 1 5 20 50 100; do
    r=$(HVM_SPIN_MS=$ms HVM_SPIN_SECS=6 ./hvm-spin print guest/Image guest/initramfs-$v.cpio.gz "hydo-boot-marker: launching shell" 2>&1)
    fp=$(echo "$r" | sed -n 's/.*fp=\([0-9a-f]*\).*/\1/p' | head -1)
    st=$(echo "$r" | sed -n 's/.*stop=\([0-9]\).*/\1/p' | head -1)
    res=$(echo "$r" | sed -n 's/.*resolved \([0-9]*\),.*/\1/p')
    kicks=$(echo "$r" | sed -n 's/.*spin kicks \([0-9]*\),.*/\1/p')
    spin=$(echo "$r" | grep -c "SPIN:")
    echo "$v ms=$ms stop=$st fp=${fp:-none} resolved=$res kicks=$kicks spin_invalid=$spin"
  done
done
