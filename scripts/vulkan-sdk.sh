#!/usr/bin/env bash
# The Vulkan headers and loader the speech model's GPU build compiles
# against, laid out the way whisper.cpp's build expects an SDK:
# vendor/vulkan-sdk/{include,lib}. .cargo/config.toml points VULKAN_SDK
# here. The system's own headers are used when it has them (the
# vulkan-headers package); otherwise they are fetched from Khronos, the
# version the installed loader speaks. Needs glslc (shaderc) to build.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)/vendor/vulkan-sdk"
mkdir -p "$root/lib"
for lib in /usr/lib/libvulkan.so /usr/lib/x86_64-linux-gnu/libvulkan.so /usr/lib64/libvulkan.so; do
    [ -e "$lib" ] && ln -sfn "$lib" "$root/lib/libvulkan.so" && break
done
[ -e "$root/lib/libvulkan.so" ] || { echo "no libvulkan.so found: install the Vulkan loader" >&2; exit 1; }
command -v glslc >/dev/null || { echo "glslc not found: install shaderc" >&2; exit 1; }
if [ -e /usr/include/vulkan/vulkan.hpp ]; then
    ln -sfn /usr/include "$root/include"
    echo "using the system's Vulkan headers"
    exit 0
fi
version="$(pkg-config --modversion vulkan 2>/dev/null || echo 1.4.341)"
if [ ! -e "$root/src/include/vulkan/vulkan.hpp" ]; then
    rm -rf "$root/src"
    git clone -q --depth 1 --branch "v$version" https://github.com/KhronosGroup/Vulkan-Headers.git "$root/src" \
        || git clone -q --depth 1 https://github.com/KhronosGroup/Vulkan-Headers.git "$root/src"
fi
ln -sfn "$root/src/include" "$root/include"
echo "Vulkan headers $version fetched into $root"
