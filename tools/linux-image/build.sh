#!/bin/sh
# Builds the Linux kernel image for the guest machine of `src/linux/` (kernel, BusyBox and the
# root file system in one file) into target/linux/Image. Needs Docker and about 15 GB of disk
# in a Docker volume. The first run builds a cross compiler and takes about half an hour; later
# runs take a few minutes.
#
#   tools/linux-image/build.sh          # build
#   tools/linux-image/build.sh clean    # delete the Docker volume
#
# What is built: the kernel configuration, toolchain configuration and patches of
# https://github.com/cnlohr/mini-rv32ima (Linux 6.8-rc1, RV32IMA, no MMU, machine mode), with a
# smaller BusyBox (busybox.fragment) and our /etc (overlay/).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
volume=pulsar-linux-build
# The commit of mini-rv32ima whose configuration is used.
commit=84858f5ecc6de9bf6fe8a1bb0d8d0def90b46a5c

if [ "${1:-}" = clean ]; then
    docker volume rm "$volume"
    exit 0
fi

mkdir -p "$root/target/linux"
docker run --rm \
    -e MINI_RV32IMA_COMMIT="$commit" \
    -v "$volume:/build" \
    -v "$here:/src:ro" \
    -v "$root/target/linux:/out" \
    debian:bookworm sh /src/in-container.sh

echo "Built $root/target/linux/Image"
