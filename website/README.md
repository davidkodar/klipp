# Klipp homepage

The homepage is ready to assemble for GitHub Pages. The repository is public; the site is not deployed yet, and no workflow deploys it.

## Preview

Install Python 3.11+ and GitHub CLI, then authenticate `gh` with an account that has write access to this repository (the media release is still a draft). From the repository root:

```bash
python3 website/prepare.py --output /tmp/klipp-website-preview
python3 -m http.server 8765 --bind 127.0.0.1 --directory /tmp/klipp-website-preview
```

Open `http://127.0.0.1:8765`. Choose a new output directory for each build; preparation refuses to overwrite existing files. It downloads the full-quality video and verifies its byte count and SHA-256 before producing a site. Only the static page, stylesheet, credits, images and verified video are included.

The preparation step runs locally and does not use GitHub Actions. The assembled folder can be archived for private review; it contains the complete site and full-quality video.

## Video storage and quality

The finished video is attached to the unpublished **Website media** release (`website-media-2026-10-05`) in this repository. It is not a tracked source file. `demo.json` pins the attachment ID, name, byte count and checksum. The release also carries `CREDITS.md`.

The film is the Klipp showcase, exported from Klipp as a 4K master. GitHub refuses files over 100 MB, so the web copy is encoded again from that master at 1920×1080, 60 fps: two-pass x264 `veryslow`, about 3.65 Mbit/s, the AAC track copied, MP4 fast-start metadata, with a full-HD WebP poster from the title shot. Before replacing it, score the copy against the master scaled to 1080p (VMAF and SSIM) and keep it only if it scores at least as well as Klipp’s own 1080p export; look at the UI text in the screen recordings too. Keep `preload="none"` so visiting the page does not automatically download the film. For an update, use a new versioned release/asset and update the manifest and page links together.

Raw footage, the editable demo project and production notes stay outside Git. Preserve the attribution in `credits.html` whenever the film is distributed.

## Publishing

The site is live at `https://davidkodar.github.io/klipp/`. It is served by GitHub Pages from the `gh-pages` branch, which holds only the assembled output, never the repository root. To publish an update, assemble into a new directory, then replace the branch's content with it:

```bash
python3 website/prepare.py --output /tmp/klipp-website-<date>
cd /tmp/klipp-website-<date> && touch .nojekyll
git init -b gh-pages && git add -A && git commit -s -m "Publish the homepage from main <sha>"
git push --force https://github.com/davidkodar/klipp.git gh-pages
```

No GitHub Actions workflow is involved.
