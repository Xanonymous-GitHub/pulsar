#!/bin/sh
# Runs inside the build container, started by build.sh. Builds the kernel image in /build (a
# Docker volume: the Linux sources need a file system that tells upper from lower case) and
# copies it to /out. The versions come from versions.env through the environment.
set -eu

export DEBIAN_FRONTEND=noninteractive
# Buildroot's host tools refuse to configure as root, which is what a container is.
export FORCE_UNSAFE_CONFIGURE=1

apt-get update -qq
apt-get install -y -qq --no-install-recommends \
    build-essential ca-certificates curl wget cpio unzip rsync bc file python3 perl \
    libncurses-dev bzip2 xz-utils patch flex bison gawk git >/dev/null

cd /build
buildroot="buildroot-$BUILDROOT_VERSION"
if [ ! -d "$buildroot" ]; then
    curl -fsSLO "https://buildroot.org/downloads/$buildroot.tar.xz"
    echo "$BUILDROOT_SHA256  $buildroot.tar.xz" | sha256sum -c -
    tar -xf "$buildroot.tar.xz"
    rm "$buildroot.tar.xz"
fi

# Our Buildroot "external tree": the configuration and the files it names. A copy, with the
# kernel version filled in.
rm -rf external && mkdir external
cp -a /src/external.desc /src/external.mk /src/Config.in /src/configs /src/board external/
sed -i "s/@LINUX_VERSION@/$LINUX_VERSION/" external/configs/pulsar_defconfig

make="make -C $buildroot O=/build/output BR2_EXTERNAL=/build/external"
$make pulsar_defconfig >/dev/null
# The kernel and BusyBox must follow our configuration files when those change; Buildroot only
# notices a new version by itself.
rm -rf output/build/linux-[0-9]* output/build/linux-custom output/build/busybox-[0-9]*
$make

cp output/images/Image /out/Image
echo "Linux $LINUX_VERSION, Buildroot $BUILDROOT_VERSION:"
ls -l /out/Image output/target/bin/busybox
