# Admin UI CSS build

`src/transport/admin_assets/daisyui.css` is generated from this directory and
committed. The Rust binary embeds the artifact with `include_str!`, so
`cargo build` and the Docker image never run Node or download packages.

## Regenerate

Requires Bun (the lockfile is `bun.lock`; npm/pnpm are not used here).

```sh
cd frontend/admin-css
bun install --frozen-lockfile
bun run build
```

`bun run build` writes `../../src/transport/admin_assets/daisyui.css` with
Tailwind's `--minify` output. Do not edit that file by hand.

To check reproducibility, build twice and compare hashes:

```sh
bun run build && sha256sum ../../src/transport/admin_assets/daisyui.css
bun run build && sha256sum ../../src/transport/admin_assets/daisyui.css
```

## What is scanned

`input.css` uses `@import "tailwindcss" source(none)` and lists exactly two
sources:

- `src/transport/admin_assets/app.js`
- `src/transport/admin_assets/index.html`

New daisyUI or Tailwind classes must appear literally in those files. Classes
assembled at runtime (for example `badge env-${value}`) only work when the
dynamic part is an app-owned class defined in `app.css`; a daisyUI modifier
built this way needs a literal mention or an explicit `@source inline(...)`
safelist entry in `input.css`.

## Versions and licenses

Versions are pinned exactly in `package.json` and locked in `bun.lock`
(daisyUI 5.8.1, Tailwind CSS 4.3.5, @tailwindcss/cli 4.3.5). License texts for
the packages that contribute to the artifact are archived in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).

When upgrading, change the exact versions, refresh `bun.lock`, rebuild, run the
admin HTTP tests, and update the version table and license archive.