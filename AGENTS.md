# AlfredPet Community

This repository is the public edition. Never add personal profiles, credentials,
private artwork or personal chat data. Original code and geometric development
assets are MIT; dependencies retain their own terms.

Use Node v24.21.0, Windows x64 MSVC Rust and Python with ReportLab, Pillow and
pypdf. Run frontend type checking, lint and tests, and native Rust tests for code
changes. Do not equate CI with physical-device acceptance.

Keep the Spanish guide at `docs/manual/manual.es.json` and
`output/pdf/AlfredPet-Manual-de-uso.pdf` current. Run
`python docs/manual/build_manual.py`, render and visually inspect every page,
record the review without removing fingerprints, and run `--check` plus
`node docs/manual/check_bundle.mjs`. The public guide uses standard PDF fonts.

Public changes arrive as validated snapshots of the shared core. Review changes
to identity adapters, profile isolation and license scope carefully. The public
application must use `AlfredPetPublic` and its own maintenance directory.
