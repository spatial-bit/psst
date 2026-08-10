# W-502: v0.1.0-alpha.1 portable assets

Status: implementation candidate; native exact-tag evidence pending

## Objective

Produce deterministic-layout portable archives, an SPDX SBOM, an internal payload manifest, and an
external SHA-256 checksum file from declared CI inputs.

## Acceptance

- Native runners build with Rust 1.89 and locked dependencies using remapped workspace paths.
- Each archive contains only `psst`, `psst-mcp`, transitional `psst-relay`, `LICENSE`, `README.md`,
  `INSTALL.md`, `SBOM.spdx.json`, and `MANIFEST.json` under one versioned target root.
- Archive timestamps, order, ownership, and modes are normalized. Repacking identical input bytes is
  byte-identical; native compiler output reproducibility is not claimed.
- Manifest hashes cover every payload except the manifest itself; external `SHA256SUMS` covers every
  published archive.
- CI scans assets for workspace paths and secret canaries before retention.

## Evidence

- Focused release-preparation tests prove deterministic re-packing for ZIP and tar.gz inputs,
  exact single-root inventories, complete payload manifests, stable SPDX generation, unsafe archive
  rejection, duplicate checksum-basename rejection, and checksum output/input collision rejection.
- The candidate workflow defines only the three supported target archives, uses locked Rust 1.89
  native builds and remapped paths, and scans packages before retaining them.
- These are source and workflow checks, not released assets. Exact signed-tag native builds,
  clean-download results, retained hashes, and independent candidate approval remain pending.
