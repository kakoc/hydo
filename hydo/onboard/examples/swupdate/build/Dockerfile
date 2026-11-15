# The device's update tools as static Linux programs, for the guest's architecture:
#   swupdate 2026.05.1 (build/swupdate.config: swupdate -i, the raw handler, U-Boot's environment)
#   fw_printenv / fw_setenv of libubootenv 0.3.8 (the U-Boot environment from Linux)
#   loopdio (loopdio.c): switches a loop device to direct I/O, for the eMMC model of RESULTS.md
# build.sh runs it once per architecture and copies /out out of the image.
FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f
RUN apk add --no-cache build-base cmake curl patch linux-headers perl bison flex ncurses-dev \
      openssl-dev openssl-libs-static zlib-dev zlib-static yaml-dev yaml-static \
      libconfig-dev libconfig-static json-c-dev bsd-compat-headers cpio e2fsprogs openssl pkgconf python3
WORKDIR /src
RUN curl -sSfL -o libubootenv-0.3.8.tar.gz https://github.com/sbabic/libubootenv/archive/refs/tags/v0.3.8.tar.gz \
 && curl -sSfL -o swupdate-2026.05.1.tar.gz https://github.com/sbabic/swupdate/archive/refs/tags/2026.05.1.tar.gz \
 && printf '%s\n' \
      '3374b5166306d976cfa971a507349101ad4becc7f1fae503abaa7957363a802d  libubootenv-0.3.8.tar.gz' \
      '26236a354e8cf21806d20a76728b3805ae49c23cfb2eb794fe91d1f0b280298e  swupdate-2026.05.1.tar.gz' \
    | sha256sum -c - \
 && tar xzf libubootenv-0.3.8.tar.gz && tar xzf swupdate-2026.05.1.tar.gz
RUN cd libubootenv-0.3.8 && cmake -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
 && make -C build -j4 ubootenv_static \
 && cp src/libuboot.h /usr/include/ && cp build/src/libubootenv.a /usr/lib/ \
 && mkdir -p /out \
 && cc -O2 -static -Isrc -Ibuild/src -o /out/fw_printenv src/fw_printenv.c build/src/libubootenv.a -lyaml -lz \
 && strip /out/fw_printenv && cp /out/fw_printenv /out/fw_setenv
COPY swupdate.config loopdio.c /src/
RUN cd swupdate-2026.05.1 && cp ../swupdate.config .config && yes '' | make oldconfig > /dev/null \
 && sed -i 's/^swupdate_unstripped: \${swupdate-ipc-lib} /swupdate_unstripped: $(ipc-lib) /' Makefile \
 && make -j4 swupdate && cp swupdate /out/swupdate && strip /out/swupdate
RUN cc -O2 -static -o /out/loopdio loopdio.c && strip /out/loopdio
RUN for p in /out/*; do ldd "$p" 2>&1 | grep -q 'Not a valid dynamic program' \
      || { echo "$p is not static" >&2; exit 1; }; done; ls -l /out
