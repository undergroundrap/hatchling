# Evidence: the standalone app page built on Windows starts (#529)

What was tested, how, and what came back, for
[#529](https://github.com/undergroundrap/hatchling/issues/529): built on
Windows, `dist/hatchling-standalone.html` never started.

| | |
|---|---|
| Base | `main` at `5145276b` |
| Tested tree | `7238ec27` |
| Date | 2026-11-21 (UTC) |
| Machine | x86_64, Windows 11; Node 22.17.0; Chromium 156 |

This record was added after the runs, in its own commit. It changes no code.

## The problem

`frontend/scripts/standalone.mjs` collected the compiled modules and the
public assets with `path.join`, which joins with `\` on Windows. It then
named each module by taking `dist/app/` off its path, and each asset by
taking `public/` off, both written with `/`. On Windows neither matched:
every module kept its `dist\app\` prefix, and no asset was inlined.

Built on Windows from `main`, the page stayed on "Opening Hatchling…", and
the browser's console said:

- `Uncaught Error: Missing preview module: main.js`;
- the page's policy (`img-src data: blob:`) blocked `favicon.ico`,
  `favicon-32x32.png`, `favicon-16x16.png` and `apple-touch-icon.png`,
  left as files instead of inlined.

The built page named its modules `"dist\\app\\main.js"` and so on: there
was no `"main.js"`.

## What changed

- **Paths are joined with `/` on every system** (`path.posix.join`), so a
  module is named, and an asset matched, by the same path wherever the
  page is built.
- **A path not under its folder stops the build,** naming the path, and
  so does a build with no module named `main.js`: either would make a
  page that can't start.
- **On Linux nothing changes:** there `path.join` is `path.posix.join`, so
  what CI and the releases build is the same.

## Results

| Check | Result |
|---|---|
| Built on Windows, opened in the browser | starts: the composer and the demo chat show, the icons are `data:` URLs |
| Its modules | named `main.js`, `app/controller.js` and so on, one for each compiled module in `dist/app` |
| New tests (`standalone.test.mjs`) | 3 passed: the builder run on a small frontend (nested modules, assets inlined), a build with no `main.js` stopping, and the real built page naming every module as its loader asks |
| `npm run check` | passed: 611 unit tests |
| Coverage, run by hand from Git Bash | the app's: 611 passed; the live adapter's: 262 passed, 100%; the operator's: 16 passed, 100% |

### Hand-breaks

Each break reran the builder and the new tests, then was undone.

| Break | Caught by |
|---|---|
| Paths joined with Windows' `\` again | the build stops: `public\apple-touch-icon.png isn't under public/` |
| The same, with the folder check taken out | the build passes, and `modules are named from dist/app with /...` and `the built standalone page names every compiled module...` fail |
| No check for `main.js` | `a build with no main.js stops...` |
| Assets matched with their `public/` folder | `modules are named...` and `the built standalone page...` (assets not inlined) |

4 broken, 4 caught. `path.win32.join` gives `\` on any system, so the
first two breaks fail in CI too, not only on Windows.

## Residual

- **The standalone page asks the host whether Hatchling is locked,** and
  its own policy (`connect-src 'none'`) blocks the request, which the
  console shows as an error. The page goes on as the demo. This happens on
  a Linux build too, and isn't part of this fix.
