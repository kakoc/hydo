# The device's update tools as static Linux programs, for the guest's architecture:
#   mender-update 5.2.0 (Mender's update client with mender-auth inside it, no D-Bus, gzip Artifacts)
#   mender-flash 1.1.0 (the writer of the inactive slot that the rootfs-image module prefers)
#   fw_printenv / fw_setenv of libubootenv 0.3.8 (U-Boot's environment from Linux)
#   busybox (Alpine's static busybox: the slots' shell and tools)
#   loopdio (loopdio.c): switches a loop device to direct I/O, so a partition of the eMMC
#   writes through to it (RESULTS.md, "The device")
#   mdb_dump of LMDB 0.9.33: the checks read Mender's database with it, without changing it
#   mender-artifact 4.4.2 (only in the native image: it writes the Artifacts on the build host)
# build.sh runs it once per architecture and copies /out out of the image. For the other
# architecture it builds only the stage `tools`, without mender-artifact. Every source is pinned
# by sha256, the base image by digest.
FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f AS tools
RUN apk add --no-cache build-base cmake curl linux-headers perl boost1.84-dev boost1.84-static \
      openssl-dev openssl-libs-static zlib-dev zlib-static yaml-dev yaml-static busybox-static \
      e2fsprogs cpio go bsd-compat-headers
WORKDIR /src
RUN curl -sSfL -o mender-5.2.0.tar.gz https://github.com/mendersoftware/mender/releases/download/5.2.0/mender-5.2.0.tar.gz \
 && curl -sSfL -o mender-flash-1.1.0.tar.gz https://github.com/mendersoftware/mender-flash/archive/refs/tags/1.1.0.tar.gz \
 && curl -sSfL -o mender-artifact-4.4.2.tar.gz https://github.com/mendersoftware/mender-artifact/archive/refs/tags/4.4.2.tar.gz \
 && curl -sSfL -o libubootenv-0.3.8.tar.gz https://github.com/sbabic/libubootenv/archive/refs/tags/v0.3.8.tar.gz \
 && curl -sSfL -o lmdb-LMDB_0.9.33.tar.gz https://github.com/LMDB/lmdb/archive/refs/tags/LMDB_0.9.33.tar.gz \
 && curl -sSfL -o libarchive-3.8.4.tar.gz https://github.com/libarchive/libarchive/releases/download/v3.8.4/libarchive-3.8.4.tar.gz \
 && printf '%s\n' \
      '436ae559358f29c672b98ca77b953936ba6ef51455466fb5ed0ced7ae77fc226  mender-5.2.0.tar.gz' \
      '185495babb4d836d9f5218ce4391f6e9bcb7cf2a7fbc8c5df4da57e7c42b8b91  mender-flash-1.1.0.tar.gz' \
      'd8e9e18e48a2124e5e367ffafb3313a3dcba7a8a8a8b162b1dcb8791964c6385  mender-artifact-4.4.2.tar.gz' \
      '3374b5166306d976cfa971a507349101ad4becc7f1fae503abaa7957363a802d  libubootenv-0.3.8.tar.gz' \
      '9c352fa9bdad53d920128ac6a62296f2850c7e29862a610d3a72f5f49a1cd7ea  lmdb-LMDB_0.9.33.tar.gz' \
      'b2c75b132a0ec43274d2867221befcb425034cd038e465afbfad09911abb1abb  libarchive-3.8.4.tar.gz' \
    | sha256sum -c - \
 && for f in *.tar.gz; do tar xzf "$f"; done
# retry runs a build command again after a compiler crash under emulation (retry.sh)
COPY retry.sh /usr/local/bin/retry
# LMDB and a small libarchive (tar and gzip only), as static libraries
RUN retry make -C lmdb-LMDB_0.9.33/libraries/liblmdb liblmdb.a XCFLAGS=-O2 \
 && cp lmdb-LMDB_0.9.33/libraries/liblmdb/lmdb.h /usr/include/ \
 && cp lmdb-LMDB_0.9.33/libraries/liblmdb/liblmdb.a /usr/lib/
RUN cmake -S libarchive-3.8.4 -B la -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
      -DENABLE_TEST=OFF -DENABLE_TAR=OFF -DENABLE_CPIO=OFF -DENABLE_CAT=OFF -DENABLE_UNZIP=OFF \
      -DENABLE_ACL=OFF -DENABLE_XATTR=OFF -DENABLE_ICONV=OFF -DENABLE_LIBXML2=OFF -DENABLE_EXPAT=OFF \
      -DENABLE_BZip2=OFF -DENABLE_LZ4=OFF -DENABLE_LZMA=OFF -DENABLE_ZSTD=OFF -DENABLE_LZO=OFF \
      -DENABLE_LIBB2=OFF -DENABLE_OPENSSL=OFF -DENABLE_MBEDTLS=OFF -DENABLE_NETTLE=OFF \
      -DENABLE_CNG=OFF -DENABLE_PCREPOSIX=OFF -DENABLE_PCRE2POSIX=OFF -DENABLE_WIN32_XMLLITE=OFF \
 && retry make -C la -j8 archive_static && make -C la install > /dev/null \
 && rm -f /usr/local/lib/libarchive.so*
RUN cd libubootenv-0.3.8 && cmake -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
 && retry make -C build -j8 ubootenv_static \
 && mkdir -p /out \
 && retry cc -O2 -static -Isrc -Ibuild/src -o /out/fw_printenv src/fw_printenv.c build/src/libubootenv.a -lyaml -lz \
 && strip /out/fw_printenv && cp /out/fw_printenv /out/fw_setenv
RUN cd mender-5.2.0 && cmake -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_TESTS=OFF -DMENDER_VERSION=5.2.0 \
      -DMENDER_USE_DBUS=OFF -DMENDER_USE_ASIO_LIBDBUS=OFF -DMENDER_EMBED_MENDER_AUTH=ON \
      -DMENDER_ARTIFACT_LZMA_COMPRESSION=OFF -DMENDER_ARTIFACT_ZSTD_COMPRESSION=OFF \
      -DBoost_USE_STATIC_LIBS=ON -DOPENSSL_USE_STATIC_LIBS=ON \
      -DCMAKE_PREFIX_PATH=/usr/local -DCMAKE_EXE_LINKER_FLAGS=-static -DCMAKE_CXX_STANDARD_LIBRARIES=-lz \
      -DCMAKE_CXX_FLAGS=-ffile-prefix-map=/src/= -DCMAKE_C_FLAGS=-ffile-prefix-map=/src/= \
 && retry make -C build -j8 mender-update \
 && cp build/src/mender-update/mender-update /out/ && strip /out/mender-update \
 && cp support/modules/rootfs-image /out/rootfs-image
RUN cd mender-flash-1.1.0 && retry cc -O2 -include libgen.h -static -o /out/mender-flash main.c && strip /out/mender-flash
RUN cp /bin/busybox.static /out/busybox
COPY loopdio.c /src/
RUN retry cc -O2 -static -o /out/loopdio loopdio.c && strip /out/loopdio
RUN retry make -C lmdb-LMDB_0.9.33/libraries/liblmdb mdb_dump XCFLAGS=-O2 LDFLAGS=-static \
 && cp lmdb-LMDB_0.9.33/libraries/liblmdb/mdb_dump /out/ && strip /out/mdb_dump
RUN for p in mender-update mender-flash fw_printenv busybox loopdio mdb_dump; do ldd "/out/$p" 2>&1 | grep -q 'Not a valid dynamic program' \
      || { echo "$p is not static" >&2; exit 1; }; done; ls -l /out
# the Artifact writer, for the build host (build.sh runs make-images.sh in the native image).
# Go's compiler crashes under QEMU's emulation, so the other architecture's image ends at `tools`.
FROM tools
# mender-artifact 4.4.2 keeps an Artifact's state scripts in a Go map and writes them in the
# map's random order, so the patch sorts them: every build then gives the same Artifacts.
COPY mender-artifact-script-order.patch /src/
RUN cd mender-artifact-4.4.2 && patch -p1 < ../mender-artifact-script-order.patch && CGO_ENABLED=1 GOFLAGS=-mod=vendor go build -trimpath -tags nopkcs11 \
      -ldflags "-X github.com/mendersoftware/mender-artifact/cli.Version=4.4.2" \
      -o /usr/local/bin/mender-artifact . \
 && mender-artifact --version
