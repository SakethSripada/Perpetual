# Perpetual Landing Page

Standalone Vite React landing site for Perpetual. It lives in `landing/` so the
marketing site can be deployed independently of the extension and Rust workspace.

## Commands

```bash
npm install
npm run dev
npm run build
npm run preview
npm run smoke:visual
```

The production artifact is `landing/dist`.

`npm run smoke:visual` builds the site, starts a local preview server, captures
desktop/laptop/mobile/reduced-motion screenshots, and checks for console errors,
horizontal overflow, missing hero render, and undersized CTAs. Screenshots are
written to `landing/artifacts/visual-smoke/`.

## Links

Edit CTA URLs in `src/content.ts`:

- `DOWNLOAD_URL`: placeholder Visual Studio Marketplace link.
- `DEMO_URL`: placeholder demo/contact link.

## Assets

The site uses product-native visuals and the local Perpetual app icon. No
external photography is included. If future external imagery is added, record the
source and license here before publishing.
