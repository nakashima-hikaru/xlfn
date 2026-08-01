# xlfn User Guide

This directory contains the canonical English user guide for xlfn.

The source is **mdBook-compatible Markdown**. This format was selected because it provides:

- a clean reading experience on GitHub without a build step;
- structured navigation, full-text search, and printable output when built with mdBook;
- reviewable, version-controlled source that can evolve with the crate;
- ordinary fenced Rust, TOML, and PowerShell examples;
- a clear separation from generated rustdoc, which remains the API-signature reference.

A single README would become difficult to navigate and would mix onboarding, concepts, operations, and reference material. A binary format such as PDF or DOCX would be less suitable as the canonical OSS source because it is harder to review in pull requests and keep synchronized with code. Generated HTML or PDF may be published from this source for releases.

> **Source snapshot:** the workspace currently sets `publish = false`. Use a repository checkout, `path` dependencies, or an audited Git revision as described in the guide. Commands using a crate version show the intended workflow after a release is published.

## Read on GitHub

Start with [`src/introduction.md`](src/introduction.md), or use [`src/SUMMARY.md`](src/SUMMARY.md) as the table of contents.

## Build locally

Install mdBook, then run:

```console
mdbook serve xlfn/guide --open
```

To produce static HTML:

```console
mdbook build xlfn/guide
```

The generated site is written to `xlfn/guide/book/` and should not be committed.

## Validate the source

The repository includes a dependency-free checker for the table of contents, local links, and trailing whitespace:

```console
python3 xlfn/guide/check.py
```

Run the normal Rust checks separately; the guide checker does not compile Rust examples.
## Documentation maintenance

Treat the guide as part of the public API:

- update the relevant concept and reference chapters in the same change as an API or CLI modification;
- keep examples strict, bounded, and free of hidden Excel or native-lifetime assumptions;
- prefer links to rustdoc over copying every method signature into prose;
- run `python3 xlfn/guide/check.py`, `mdbook build xlfn/guide`, and the applicable Rust and Windows artifact checks before publishing;
- publish generated HTML from CI or release automation, but do not commit `xlfn/guide/book/`.
