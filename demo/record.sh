#!/bin/bash
# Record the demo scene and render it for the README (GIF) and for social
# posts (MP4 that opens on the hero image, so its first frame is a title card).
# Needs asciinema (uv tool install asciinema), agg (cargo install --git
# https://github.com/asciinema/agg) and ffmpeg. Builds a fresh demo first.
# Usage: demo/record.sh            -> docs/assets/demo.gif, docs/assets/demo.mp4
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
assets="$here/../docs/assets"
DEMO=/tmp/memsaga-demo
"$here/setup.sh" "$DEMO" >/dev/null
cast="$DEMO/history.cast"
# shellcheck disable=SC1091
( source "$DEMO/env.sh"
  asciinema rec --overwrite --quiet --cols 104 --rows 28 -c "$here/scene-history.sh" "$cast" )
agg --font-size 20 --theme asciinema --idle-time-limit 3 --last-frame-duration 3 "$cast" "$assets/demo.gif"
# MP4: 2 s of the hero image as the title card, then the recording.
ffmpeg -loglevel error -y -i "$assets/demo.gif" -movflags faststart -pix_fmt yuv420p \
  -vf "fps=25,scale=trunc(iw/2)*2:trunc(ih/2)*2" "$DEMO/scene.mp4"
IFS=x read -r w h < <(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=p=0:s=x "$DEMO/scene.mp4")
ffmpeg -loglevel error -y -loop 1 -t 2 -i "$assets/social-preview.png" -r 25 -pix_fmt yuv420p \
  -vf "scale=$w:$h:force_original_aspect_ratio=decrease,pad=$w:$h:(ow-iw)/2:(oh-ih)/2:color=white" "$DEMO/title.mp4"
printf "file '%s'\nfile '%s'\n" "$DEMO/title.mp4" "$DEMO/scene.mp4" > "$DEMO/list.txt"
ffmpeg -loglevel error -y -f concat -safe 0 -i "$DEMO/list.txt" -c:v libx264 -pix_fmt yuv420p -movflags faststart "$assets/demo.mp4"
ls -l "$assets/demo.gif" "$assets/demo.mp4"
