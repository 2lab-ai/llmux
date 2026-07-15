# macOS and KDE visual receipts

These 11 PNGs are durable production-renderer evidence for the asymmetric
presentation contract: macOS preserves its pre-renewal native UI, while KDE
uses the aligned monochrome OpenAI-style shell and keeps infrequent controls in
Linux-only `Advanced` disclosures.

The files came from [Islands parity run
29397360302](https://github.com/2lab-ai/llmux/actions/runs/29397360302) at source
commit
[`7c321ff908ce6ed4d544d2c76a2f30130cb3ea9a`](https://github.com/2lab-ai/llmux/commit/7c321ff908ce6ed4d544d2c76a2f30130cb3ea9a).
That run passed the semantic core, generated and Xcode-tested the macOS app,
and built, packaged, installed, and smoke-tested the KDE shell in clean Arch
before uploading both screenshot artifacts.

The four macOS files are byte-identical to the corresponding captures from the
[`57df760cb57ba811d2a29bcddeb149eb7c4a04ad`](https://github.com/2lab-ai/llmux/commit/57df760cb57ba811d2a29bcddeb149eb7c4a04ad)
pre-renewal presentation boundary. macOS therefore has no synthetic
`Advanced` screenshots. KDE supplies default/Advanced full-shell pairs for its
three routes plus a readable receipt surface.

Both renderers use the same deterministic, privacy-masked dashboard fixture.
Account email addresses are anonymized, API keys and request/response bodies
are excluded, and unsafe note content is rendered as `[REDACTED]`. Receipt
captures expose only request metadata and a successful settings-mutation
daemon-readback receipt.

| Platform | Surface | File | Dimensions | SHA-256 |
|---|---|---|---:|---|
| macOS | Usage, original | `macos-usage-full.png` | 1200x956 | `3f653fa09966ee7d200946cc8054ad461db35f4f15808cfb2bf0a31648fccd67` |
| macOS | Statistics, original | `macos-statistics-full.png` | 1200x1280 | `4091860d18e840c631e11c0b1e543fabb62890418c854a625a471d866a8fc908` |
| macOS | Request and verification receipt | `macos-receipts-detail.png` | 1120x520 | `65503f3060307c98402b37b424ba076bee0f626127949c0a71f1d60319468164` |
| macOS | Menu, original | `macos-menu-full.png` | 960x1640 | `2a6f0e7c9c19b2980916e477d8504bac16f5e053c063a1a069c95a32f8bb60ff` |
| KDE | Usage, default | `kde-usage-full.png` | 960x760 | `a107f32f938aee533c9410e7a76bc9beb47e096eaef38de6fb25da37678a4ffe` |
| KDE | Usage, Advanced | `kde-usage-advanced-full.png` | 960x760 | `1520ce0a4af8a6a8757e3c8d43c021f410e16c95c1d89aa3515b9cf84fdad5ea` |
| KDE | Statistics, default | `kde-statistics-full.png` | 960x760 | `7cb47337e4a770ab3728a5b65461aa12dc9108e6bc54d5e232e6f76da3483a7a` |
| KDE | Statistics, Advanced | `kde-statistics-advanced-full.png` | 960x926 | `6d72d7aa230b7f8adbe5d9e453b5c8eec4dc8486c7f763034b70a6489e630eba` |
| KDE | Request and verification receipts | `kde-receipts-detail.png` | 960x973 | `e4dd3a98504a77a14eb6fd8cfd65313327be13a77deaefa18ad2ca849c5ec662` |
| KDE | Settings, default | `kde-menu-full.png` | 960x760 | `810cfc2a778fafa289dda93a59f17d9eb17c8460dfe6485b6ce3261db8645e22` |
| KDE | Settings, Advanced | `kde-menu-advanced-full.png` | 960x1630 | `574dbf00f1ca0e7cf66f1efb7df469921d8121a532f74762da63e7b591760b66` |

All 11 hashes match their CI artifact manifests. Every PNG was inspected at
original resolution for complete shell chrome, content bounds, masking,
receipt visibility, default/Advanced hierarchy where applicable, and secret
exclusion.
