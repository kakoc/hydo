#!/bin/sh
# mkimg.sh: overlay each macrace-<v> build onto ../guest/initramfs.cpio.gz as initramfs-race-<v>.cpio.gz
cd "$(dirname "$0")"
for v in mem clk mem2 clk2; do
	rm -rf ov && mkdir -p ov/bin && cp macrace-$v ov/bin/macrace
	printf '#!/bin/sh\nmount -t proc proc /proc\nmount -t devtmpfs dev /dev 2>/dev/null\necho "hydo-race-start"\n/bin/macrace\necho "hydo-race-done"\nexec sh\n' > ov/init
	chmod 755 ov/init ov/bin/macrace
	(cd ov && find . | LC_ALL=C sort | cpio -o -H newc --quiet 2>/dev/null) | gzip -n -9 > ov-$v.cpio.gz
	cat ../guest/initramfs.cpio.gz ov-$v.cpio.gz > initramfs-race-$v.cpio.gz
done
rm -rf ov ov-*.cpio.gz
