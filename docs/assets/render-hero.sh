#!/bin/sh
# Render docs/assets/hero.html to hero-light.png and hero-dark.png (2560 x 1280,
# for the README) and social-preview.png (1280 x 640, light, for the GitHub
# repository's Settings > Social preview). Needs Google Chrome and network
# access for the web fonts. Usage: docs/assets/render-hero.sh
set -eu
dir=$(cd "$(dirname "$0")" && pwd)
chrome=${CHROME:-"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
shot() { # theme scale output
  "$chrome" --headless=new --disable-gpu --hide-scrollbars --window-size=1280,640 \
    --force-device-scale-factor="$2" --virtual-time-budget=5000 \
    --screenshot="$dir/$3" "file://$dir/hero.html?theme=$1" >/dev/null 2>&1
}
shot light 2 hero-light.png
shot dark 2 hero-dark.png
shot light 1 social-preview.png
ls -l "$dir"/hero-light.png "$dir"/hero-dark.png "$dir"/social-preview.png
