#!/bin/sh
# Three synthetic 20 s clips at 25 fps, 640x360, each a flat colour with its
# letter and its own frame number burned in, and a tone of its own pitch:
# a cut or trim can be checked to the frame, and nothing personal is in them.
set -e
mkdir -p "$(dirname "$0")/fixtures"
cd "$(dirname "$0")/fixtures"
mk() { # name colour freq
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "color=c=$2:s=640x360:r=25:d=20" \
    -f lavfi -i "sine=frequency=$3:sample_rate=48000:duration=20" \
    -vf "drawtext=text='$1 %{frame_num}':start_number=0:fontsize=96:fontcolor=white:x=(w-tw)/2:y=(h-th)/2" \
    -c:v libx264 -pix_fmt yuv420p -g 25 -c:a aac -shortest "$1.mp4"
}
mk A 0x803020 440
mk B 0x205080 660
mk C 0x307030 880
