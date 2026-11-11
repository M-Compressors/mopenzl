# Documentation publishing

The repository publishes a static **mdBook** through GitHub Pages. The book
contains every Markdown page and associated asset in `docs/` and `architecture/`.
These directories remain the canonical sources; generated book sources, Mermaid
assets, consumer examples, and HTML are local artifacts under ignored `target/`.

## Modules and ownership

```mermaid
graph TD
    Guides[docs: user guides, examples, benchmark data] --> Prepare[scripts/docs.py: prepare]
    Specs[architecture: specifications and index] --> Prepare
    Index[architecture/README.md: architecture navigation and section pages] --> Prepare
    Prepare --> Source[target/book-src: generated Markdown and assets]
    CSV[Canonical Silesia CSV] --> Derive[benchmark_pages: per-graph encoder/decoder chapters]
    Prepare --> Derive
    Derive --> Source
    Mermaid[mdbook-mermaid 0.17.1: bundled JS] --> Assets[target/book-assets]
    Config[book.toml, book-theme/custom.css and diagram-zoom.js] --> Build[mdbook 0.5.4]
    Source --> Build
    Assets --> Build
    Build --> HTML[target/book: static HTML and search]
    HTML --> Check[Python local-link and anchor validation]
    Check --> Pages[GitHub Pages artifact]
```

The root `book.toml` owns renderer, theme, search, folding, output path, and
Mermaid preprocessor settings. `book-theme/custom.css` overrides the palette of
mdBook's `light` and `navy` themes and styles tables, the sidebar and the
landing page. `book-theme/diagram-zoom.js` moves a rendered Mermaid SVG into a
modal dialog at its natural size and returns it to its page position on close;
it borrows the node because Mermaid scopes diagram styles by the SVG's id. Search, keyboard navigation,
theme selection, Rust syntax highlighting, and code copying come from mdBook.
Rust playground execution is disabled because its environment lacks mopenzl.

`scripts/docs.py` has five repository-local entry points:

| Command | Behavior |
| --- | --- |
| `prepare` | Recreate the staged source tree, navigation, and bundled diagram assets |
| `build` | Prepare, build HTML, then check local links and heading anchors |
| `serve` | Prepare, then serve the generated tree on loopback port 3000 |
| `check-links` | Validate a previously built HTML tree |
| `test-examples` | Compile and run Rust recipes in a separate Cargo consumer |

## Source and link flow

```mermaid
flowchart TD
    Copy[Copy docs and architecture] --> Links[Inspect destinations outside fenced code]
    Links --> Local{Target in published directories?}
    Local -->|yes| Relative[Keep relative link]
    Local -->|no| GitHub[Link to source on GitHub main]
    Relative --> Summary[Generate SUMMARY with every Markdown page and the section pages]
    GitHub --> Summary
    Summary --> Complete{All source pages indexed?}
    Complete -->|no| Fail[Fail preparation]
    Complete -->|yes| Render[mdBook preprocessors and HTML renderer]
    Render --> Validate[Validate href and src paths, HTML IDs and README site URLs]
    Validate --> Artifact[Publishable static directory]
```

The generated `SUMMARY.md` has six parts: **Start here** (getting started,
the API chooser in `docs/README.md`, examples), **Graphs and codecs** (graphs,
codecs, structured data, dictionaries), **Guides**, **Performance and
correctness**, **How it works**, and **Contributing**. `GUIDE_PARTS` and
`CONTRIBUTING` in `scripts/docs.py` hold the curated order; a tuple nests its
tail under its first page, as the I/O and Sans I/O guides nest under streaming
and the graph recipes under framing containers. `book.toml` folds every nested
level, so the sidebar opens with top-level chapters only and mdBook expands
the branch of the current page. Encoder/decoder benchmark indexes refer to the actual OpenZL graph
sweep; raw CSV and per-graph charts are published unchanged. The most recent
committed Silesia after.csv also generates individual encoder/decoder graph
chapters at build time. Their paired Rust/C sizes are validated, and each table
links to its source measurement record. Canonical Markdown overrides a derived
page with the same path; derived pages do not duplicate maintained source text.
Architecture chapters follow the section headings and specification tables in
`architecture/README.md`. For each `##` section that contains a specification
table, preparation writes a generated landing page
`architecture/sections/<slug>.md` holding that section's text, with links
rewritten for its location, and nests the section's specifications under it.
Unindexed architecture Markdown fails preparation.
New guide pages are appended to **Guides** automatically. No source guide or specification is
excluded merely because it has no manually configured navigation entry.

Preparation preserves fenced code. Relative links to files outside the published
directories are checked for existence and rewritten to GitHub `blob` or `tree`
URLs. Staging maps README pages to `index.md` and the introduction to the root
`index.md`, adjusting relative links and summary entries to their final locations.
CSV, JSON, and SVG assets retain their source directory layout. All generated
HTML local destinations and fragments are checked after rendering. The root
`README.md` is not published, but it links into the site: the same check
resolves each of its `https://m-compressors.github.io/mopenzl/` URLs, with its
fragment, against the built HTML.

The preprocessor's `install` command writes bundled Mermaid assets into a small
generated book under `target/book-assets/`; the real book references those files.
The site needs no runtime CDN, Node package manager, or application server.

## Navigation

```mermaid
graph LR
    Book[SUMMARY.md] --> Overview[Overview: docs/introduction.md]
    Book --> Start[Start here]
    Book --> Concepts[Graphs and codecs]
    Book --> Guides[Guides]
    Book --> Evidence[Performance and correctness]
    Book --> Internals[How it works]
    Book --> Contributing[Contributing]
    Start --> StartPages[getting started, API chooser, examples]
    Concepts --> ConceptPages[graphs, codecs, structured data, dictionaries]
    Guides --> GuidePages[configuration, memory, decompression, streaming, parallel, frames]
    GuidePages --> Nested[folded: I/O and Sans I/O under streaming, graph recipes under frames]
    Evidence --> Bench[correctness, benchmark index]
    Bench --> Graph[folded: encoder and decoder matrices, derived per-graph pages, run records]
    Internals --> Sections[overview, generated section pages]
    Sections --> Specs[folded: specifications from architecture/README.md tables]
    Contributing --> Dev[development, benchmarking, website]
```

## Examples and deployment

```mermaid
sequenceDiagram
    participant Source as Markdown recipes
    participant Consumer as Temporary Cargo consumer
    participant CI as Pages build job
    participant Deploy as Pages deploy job
    Source->>Consumer: Extract every rust fence from all user-guide recipe chapters
    Consumer->>Consumer: Release build with local mopenzl + framed + Rayon
    Consumer->>CI: Run programs and round-trip assertions
    CI->>CI: Build book and check local links and anchors
    CI->>Deploy: Upload static Pages artifact
    alt main push or manual run on main
        Deploy->>Deploy: Configure Pages and deploy with OIDC
    else pull request
        Deploy->>Deploy: Skip publication
    end
```

Every fenced `rust` block under `docs/`, including the landing page and the
configuration, memory, decompression and streaming guides, is a complete Rust
program. `test-examples` generates a separate package beneath
`target/book-examples/`, enables default std codecs plus `framed`, and adds
Rayon. It runs each binary and propagates build or assertion failures. It does
not change the workspace, public codec APIs, state machines, SIMD dispatch, or
error propagation; normal codec tests remain separate.

`.github/workflows/pages.yml` runs source and recipe checks on matching pull
requests, `main` pushes, and manual dispatch. Only its deployment job has
`pages: write` and `id-token: write`; pull requests cannot deploy. The
`github-pages` environment records the deployed URL. Deployment is serialized
and the output is hosted at `https://m-compressors.github.io/mopenzl/`.

## Known gaps

- The repository is configured for the **GitHub Actions** Pages build type. A
  fork must enable it independently; first publication still requires this
  workflow on main and a successful build/deploy run.
- `serve` watches generated files. Restart the command after editing canonical
  sources so preparation runs again.
- The checker validates local destinations and anchors, not external network
  URLs or Mermaid syntax. Browser QA is needed for rendered diagrams.
- The diagram dialog and theme have no automated test; they were checked in a
  desktop browser.
- Recipe validation covers all Rust programs in docs/, not historical
  code fragments in implementation specifications. In-workspace generated
  tests cover the README and the guides listed in `scripts/gen_docs_examples.py`; the book consumer additionally
  tests Rayon/framed/std recipes without adding Rayon to production dependencies.
