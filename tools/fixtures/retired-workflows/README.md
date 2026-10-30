# Retired workflow fixtures

These exact workflow definitions were copied byte for byte from `.github/workflows/` in Nucleus dev source commit `fc73557bf5b1a63459bc3b67b0eb6de36d229965`, before retirement in PR #24. The remaining files are offline inputs for local classification, cache, image and runtime regressions. Admission and recovery fixtures were removed with their retired controllers. These files are not Actions entrypoints or a contract for new CI.

Keep protocol path strings in external recovery records unchanged. Historical controller code and fixtures remain available from Git. Do not restore these definitions as new Actions workflows or rebind an old operation to a new source.
