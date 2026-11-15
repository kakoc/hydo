# The device's update tools as static Linux programs, for the guest's architecture:
#   rauc 1.15.2 (no D-Bus service, no network or streaming: `rauc install` runs in its own process)
#   fw_printenv / fw_setenv of libubootenv 0.3.8 (the U-Boot environment from Linux)
#   unsquashfs of squashfs-tools 4.6.1 (only the Mac engine needs it; see rauc-unsquashfs.patch)
# build.sh runs it once per architecture and copies /out out of the image.
FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f
RUN apk add --no-cache build-base meson ninja pkgconf cmake curl xz patch \
      glib-dev glib-static openssl-dev openssl-libs-static zlib-dev zlib-static \
      libffi-dev pcre2-dev pcre2-static util-linux-dev util-linux-static libeconf-dev yaml-dev \
      yaml-static gettext-static squashfs-tools e2fsprogs openssl \
 && echo 'Libs.private: -leconf' >> /usr/lib/pkgconfig/mount.pc
WORKDIR /src
RUN curl -sSfLO https://github.com/rauc/rauc/releases/download/v1.15.2/rauc-1.15.2.tar.xz \
 && curl -sSfL -o libubootenv-0.3.8.tar.gz https://github.com/sbabic/libubootenv/archive/refs/tags/v0.3.8.tar.gz \
 && curl -sSfL -o squashfs-tools-4.6.1.tar.gz https://github.com/plougher/squashfs-tools/archive/refs/tags/4.6.1.tar.gz \
 && printf '%s\n' \
      '127a24cde208c65b837ae978c695a00730f1094ee8b6c7d48cf58ef846eae340  rauc-1.15.2.tar.xz' \
      '3374b5166306d976cfa971a507349101ad4becc7f1fae503abaa7957363a802d  libubootenv-0.3.8.tar.gz' \
      '9c4974e07c61547dae14af4ed1f358b7d04618ae194e54d6be72ee126f0d2f53  squashfs-tools-4.6.1.tar.gz' \
    | sha256sum -c - \
 && tar xf rauc-1.15.2.tar.xz && tar xzf libubootenv-0.3.8.tar.gz && tar xzf squashfs-tools-4.6.1.tar.gz
COPY rauc-unsquashfs.patch /src/
RUN cd rauc-1.15.2 && patch -p1 < ../rauc-unsquashfs.patch \
 && meson setup build --buildtype=release --default-library=static --prefer-static \
      -Dc_args=-Dread_file=rauc_read_file -Dc_link_args=-static -Dservice=false -Dnetwork=false -Dstreaming=false -Djson=disabled \
      -Dgpt=disabled -Dcomposefs=disabled -Dpkcs11_engine=false -Dtests=false \
 && ninja -C build \
 && mkdir -p /out && cp build/rauc /out/rauc && strip /out/rauc
RUN cd libubootenv-0.3.8 && cmake -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
 && make -C build -j4 ubootenv_static \
 && cc -O2 -static -Isrc -Ibuild/src -o /out/fw_printenv src/fw_printenv.c build/src/libubootenv.a -lyaml -lz \
 && strip /out/fw_printenv
RUN cd squashfs-tools-4.6.1/squashfs-tools \
 && make -j4 unsquashfs GZIP_SUPPORT=1 XZ_SUPPORT=0 LZO_SUPPORT=0 LZ4_SUPPORT=0 ZSTD_SUPPORT=0 \
      XATTR_SUPPORT=0 LDFLAGS=-static \
 && cp unsquashfs /out/unsquashfs && strip /out/unsquashfs
RUN file_ok() { ! ldd "$1" >/dev/null 2>&1 || ldd "$1" 2>&1 | grep -q 'Not a valid dynamic program'; } ; \
    for p in /out/*; do file_ok "$p" || { echo "$p is not static" >&2; ldd "$p"; exit 1; }; done; ls -l /out
