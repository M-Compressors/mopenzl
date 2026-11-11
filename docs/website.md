# Documentation website

The [mopenzl book](https://m-compressors.github.io/mopenzl/) is built with
[mdBook](https://rust-lang.github.io/mdBook/) and published through GitHub Pages.
All pages in `docs/` and `architecture/` are included, along with their charts,
CSV measurements, and environment records. Edit those files directly: there is
no second maintained copy of the documentation.

## Build locally

Requires Python **3.10+** and the pinned book tools:

```sh
cargo install mdbook --version 0.5.4 --locked
cargo install mdbook-mermaid --version 0.17.1 --locked
python3 scripts/docs.py build
```

Open `target/book/index.html`, or serve the book with:

```sh
python3 scripts/docs.py serve
```

The server listens on `http://127.0.0.1:3000`. Stop and rerun it after editing
source Markdown: the server watches the generated source tree rather than the
canonical directories. No Node.js installation or runtime CDN is needed.
Mermaid assets come from the pinned preprocessor and are served with the book.

## How source files become pages

`scripts/docs.py` copies `docs/` and `architecture/` into ignored
`target/book-src/`, creates a table of contents, extracts Mermaid's bundled
JavaScript into `target/book-assets/`, then invokes `mdbook build` using the root
`book.toml`. The output goes to `target/book/`. In the staged copy, README pages
are renamed to `index.md` and the introductory page becomes the root `index.md`;
their links are rewritten to match.

Graphs, codecs and typed inputs lead the curated guide navigation. Getting
started, recipes, and benchmarks follow them. Architecture
entries follow the tables in `architecture/README.md`. Individual encoder and
decoder graph chapters are generated directly from the latest committed
Silesia `after.csv`; no second table source is maintained. Every source Markdown
file must appear in the table of contents: a missing architecture entry fails
preparation. New guide pages are included automatically at the end of the guide
navigation until they receive a curated position.

Relative links between published pages stay local. Links to source files outside
the published directories point to GitHub's `main` branch. Code fences are
preserved. The staged index pages become `index.html` pages. The build checks
local files **and heading anchors** after rendering,
including HTML links, images, and downloaded benchmark data.

## Check the examples

```sh
python3 scripts/docs.py test-examples
```

This compiles and runs each Rust program in getting started, parallel, frames,
Sans I/O compression/decompression, and the I/O guide against the local crate
with default codecs, std and the opt-in `framed` feature, plus Rayon.
The same check includes graph, codec, typed-data, dictionary and API-guide programs. Programs run their documented round-trip and API assertions. Temporary consumer source and
build products remain under `target/book-examples/`.

## Publish with GitHub Pages

This repository uses **GitHub Actions** as its Pages source. For a new fork,
select **Pages → Build and deployment → Source → GitHub Actions** once in
repository settings. `.github/workflows/pages.yml` then:

1. Builds and validates the book on pull requests and pushes to `main`.
2. Compiles and runs the documented recipes.
3. Uploads the static book as a Pages artifact.
4. Deploys only on `main` pushes or a manual run on `main`.

Pull requests validate without publishing or receiving Pages write permissions.
Deployment uses the `github-pages` environment and GitHub's Pages actions;
there is no generated-content branch. The site lives under `/mopenzl/`, and
relative asset links support that prefix and local previews.

See [documentation architecture](../architecture/documentation.md) for the
source/build boundaries and the [CI specification](../architecture/ci.md).

GitHub documents the [Pages workflow build type](https://docs.github.com/en/rest/pages/pages#create-a-github-pages-site) and its repository permissions.
