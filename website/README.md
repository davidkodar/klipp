# Klipp homepage

A static, locally previewed homepage. No deployment workflow is enabled. Keep it private until publication is explicitly requested; GitHub Pages can expose a site even when its source repository is private.

From this directory:

```bash
python3 -m http.server 8765 --bind 127.0.0.1
```

Open `http://127.0.0.1:8765`. No dependencies or build step are required. The page uses relative asset URLs, so it can later be hosted at a GitHub Pages project path.

Before a public launch, review repository-access wording and links, remove the `noindex` directives deliberately, and choose the publishing source. `noindex` is not access control. Nothing here enables Pages or publishes automatically.

`assets/` contains final screenshots, the existing Klipp icon, and a compressed finished demo. Raw footage, the editable demo project, and production notes are kept outside this repository. Preserve the attribution in `credits.html` with the demo.
