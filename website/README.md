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

The finished video is attached to the unpublished **Website media** release (`website-media-2026-10-05`) in this private repository. It is not a tracked source file. `demo.json` pins the attachment ID, name, byte count and checksum. The release also carries `CREDITS.md`.

The demo preserves Klipp’s 1920×1080 H.264 CRF 18 export, with MP4 fast-start metadata and a lossless full-HD poster. Keep `preload="none"` so visiting the page does not automatically download the film. Do not force the video into a fixed small file size: compare every shot against the viewer, especially foliage and the closing hillside, before replacing it. For an update, use a new versioned release/asset and update the manifest and page links together.

Raw footage, the editable demo project and production notes stay outside Git. Preserve the attribution in `credits.html` whenever the film is distributed.

## Public launch, when explicitly approved

Review the documentation, imagery, media credits, source-access wording and repository contents first. Repository visibility and Pages publication are separate decisions; changing visibility alone does not deploy this homepage.

When approved, use the same preparation step to create a Pages deployment artifact, then add an explicit Pages deployment workflow/environment. Only the assembled output should be published, never the repository root. The workflow can fetch the private release attachment with authentication during the build; visitors then receive the video from the deployed site without a GitHub token or private release URL.

Review and remove `noindex` deliberately at launch. It is not access control. GitHub Pages normally publishes a public website even if the source repository is private; private-repository Pages also requires a supporting GitHub plan. The intended project URL is `https://davidkodar.github.io/klipp/`, but it is not accessible until deployment is enabled and succeeds.
