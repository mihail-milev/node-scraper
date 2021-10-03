#!/bin/bash

set -o errexit

container=$(buildah from scratch)
mountpoint=$(buildah mount $container)

mkdir -p $mountpoint/var/lib/pacman
pacman -Syr $mountpoint --noconfirm procps-ng net-tools

cp ./target/release/graftopstat $mountpoint/
chmod a+x $mountpoint/graftopstat

buildah config --user 1000:1000 $container
buildah config --entrypoint "/graftopstat" $container

buildah commit --format docker $container graftopstat
buildah unmount $container