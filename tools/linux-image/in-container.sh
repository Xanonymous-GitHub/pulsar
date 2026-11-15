#!/bin/sh
# Runs inside the build container, started by build.sh. Builds the kernel image in /build (a
# Docker volume: the Linux sources need a file system that tells upper from lower case) and
# copies it to /out.
set -eu

export DEBIAN_FRONTEND=noninteractive
# Buildroot's host tools refuse to configure as root, which is what a container is.
export FORCE_UNSAFE_CONFIGURE=1

apt-get update -qq
apt-get install -y -qq --no-install-recommends \
    build-essential git ca-certificates wget curl cpio unzip rsync bc file python3 perl \
    libncurses-dev bzip2 xz-utils patch flex bison gawk >/dev/null

cd /build
if [ ! -d mini-rv32ima ]; then
    git clone -q https://github.com/cnlohr/mini-rv32ima
fi
git -C mini-rv32ima checkout -q "$MINI_RV32IMA_COMMIT"
if [ ! -d buildroot ]; then
    git clone -q --recurse-submodules --depth 1 https://github.com/cnlohr/buildroot
fi
echo "buildroot fork at $(git -C buildroot rev-parse HEAD)"

# The configuration of mini-rv32ima: an RV32IMA no-MMU kernel with its root file system inside,
# a uClibc toolchain that makes "flat" binaries, and BusyBox.
configs=mini-rv32ima/configs
cp "$configs/custom_kernel_config" buildroot/kernel_config
cp "$configs/buildroot_config" buildroot/.config
cp "$configs/busybox_config" buildroot/busybox_config
cp "$configs/uclibc_config" buildroot/uclibc_config
cp "$configs/uclibc_config" buildroot/uclibc_config_extra

# Our changes: a smaller BusyBox and our own /etc.
rm -rf /build/pulsar && mkdir /build/pulsar
cp -a /src/busybox.fragment /src/overlay /build/pulsar/
set_option() {
    sed -i "s|^$1=.*|$1=\"$2\"|" buildroot/.config
    grep -q "^$1=\"$2\"\$" buildroot/.config || echo "$1=\"$2\"" >> buildroot/.config
}
set_option BR2_PACKAGE_BUSYBOX_CONFIG_FRAGMENT_FILES /build/pulsar/busybox.fragment
set_option BR2_ROOTFS_OVERLAY /build/pulsar/overlay
set_option BR2_TARGET_GENERIC_HOSTNAME pulsar
set_option BR2_TARGET_GENERIC_ISSUE "Welcome to Linux on pulsar"

make -C buildroot olddefconfig >/dev/null
# BusyBox and the root file system must follow our files when they change.
rm -rf buildroot/output/build/busybox-* buildroot/output/target/etc/inittab
make -C buildroot

cp buildroot/output/images/Image /out/Image
ls -l /out/Image buildroot/output/target/bin/busybox
