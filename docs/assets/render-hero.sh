#!/bin/sh
# Render the README's pictures, in light and dark, from their sources here:
#   hero.html          -> hero-light.png, hero-dark.png (2560 x 1280) and
#                         social-preview.png (1280 x 640, light, for the GitHub
#                         repository's Settings > Social preview)
#   sync-sequence.mmd  -> sync-sequence-light.png, sync-sequence-dark.png
# Static images rather than GitHub's own mermaid rendering, which clipped node
# labels and overlays zoom controls on the picture. Needs Google Chrome,
# Node (npx fetches @mermaid-js/mermaid-cli) and network access for web fonts.
# Usage: docs/assets/render-hero.sh
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
npx -y @mermaid-js/mermaid-cli -q -i "$dir/sync-sequence.mmd" -o "$dir/sync-sequence-light.png" -t base -c "$dir/mermaid-light.json" -b '#ffffff' -s 2
npx -y @mermaid-js/mermaid-cli -q -i "$dir/sync-sequence.mmd" -o "$dir/sync-sequence-dark.png" -t base -c "$dir/mermaid-dark.json" -b '#0d1117' -s 2
ls -l "$dir"/*.png
