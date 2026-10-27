# Retired workflow fixtures

These exact workflow definitions were retired from `.github/workflows/` at Nucleus dev commit `fc73557bf5b1a63459bc3b67b0eb6de36d229965`. They are offline test inputs for retained classification, cache, image, admission and recovery regressions. They are not GitHub Actions entrypoints, a deployment source or evidence of current CI enforcement.

Keep protocol path strings in recovery records unchanged. Tests read these fixture files explicitly; production controllers retain their original expected workflow identities so that historical operations are never rebound to a different source. Do not copy these definitions back into Actions or invoke their credential-bearing operations without a separately accepted deployment.
