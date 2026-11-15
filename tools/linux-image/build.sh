#!/bin/sh
# Builds the Linux kernel image for the guest machine of `src/linux/` (kernel, BusyBox and the
# root file system in one file) into target/linux/Image. Needs Docker and about 10 GB of disk
# in a Docker volume. The first run builds a cross compiler and takes about half an hour; later
# runs take a few minutes.
#
#   tools/linux-image/build.sh          # build
#   tools/linux-image/build.sh clean    # delete the Docker volume (to start from nothing)
#
# What is built is in this directory: versions.env (the kernel and Buildroot versions),
# configs/pulsar_defconfig (Buildroot), board/pulsar/ (the kernel configuration, the BusyBox
# changes and the files of /etc). Everything comes from buildroot.org and kernel.org: no patches.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
volume=pulsar-linux-build

if [ "${1:-}" = clean ]; then
    docker volume rm "$volume"
    exit 0
fi

mkdir -p "$root/target/linux"
docker run --rm \
    --env-file "$here/versions.env" \
    -v "$volume:/build" \
    -v "$here:/src:ro" \
    -v "$root/target/linux:/out" \
    debian:bookworm sh /src/in-container.sh

echo "Built $root/target/linux/Image"
