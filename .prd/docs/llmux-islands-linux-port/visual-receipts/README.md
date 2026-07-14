# macOS and KDE visual receipts

These PNGs are durable, reviewable evidence for the native macOS shell and the
Arch Linux/KDE port. They were captured from the production renderers with the
same deterministic, privacy-masked dashboard fixture at source commit
[`f87d3246843a2a4851e70229ccd0806886f219a6`](https://github.com/2lab-ai/llmux/commit/f87d3246843a2a4851e70229ccd0806886f219a6)
by [Islands parity run 29355985773](https://github.com/2lab-ai/llmux/actions/runs/29355985773).
The run Xcode-tested the macOS application and built, packaged, installed, and
smoke-tested the KDE shell in a clean Arch environment before publishing the
captures.

The fixture masks account email addresses, excludes API keys, never renders
request or response bodies, and replaces unsafe note content with
`[REDACTED]`. On both platforms, the statistics and receipt-detail captures show
recent request metadata together with the successful settings-mutation
readback receipt.

| Platform | Surface | File | Dimensions | SHA-256 |
|---|---|---|---:|---|
| macOS | Usage, full surface | `macos-usage-full.png` | 1200x956 | `3f653fa09966ee7d200946cc8054ad461db35f4f15808cfb2bf0a31648fccd67` |
| macOS | Statistics, full surface | `macos-statistics-full.png` | 1200x1280 | `4091860d18e840c631e11c0b1e543fabb62890418c854a625a471d866a8fc908` |
| macOS | Request and verification receipts | `macos-receipts-detail.png` | 1120x520 | `65503f3060307c98402b37b424ba076bee0f626127949c0a71f1d60319468164` |
| macOS | Menu, full surface | `macos-menu-full.png` | 960x1640 | `2a6f0e7c9c19b2980916e477d8504bac16f5e053c063a1a069c95a32f8bb60ff` |
| KDE | Usage, full surface | `kde-usage-full.png` | 960x760 | `6671d079b42a22f2a6e5ed5aa32c01dd736527c5f17f54780767fe7da313c3f8` |
| KDE | Statistics, full surface | `kde-statistics-full.png` | 960x1135 | `59c15e76df221b1d57752bd73e8c7767593a95a100f73b8437c810bb347e0a30` |
| KDE | Request and verification receipts | `kde-receipts-detail.png` | 924x361 | `9b9e4998d2b71e52364c6d0b863214671b6b7e9b053c9c55d21373c5091a9e46` |
| KDE | Menu, full surface | `kde-menu-full.png` | 960x1140 | `37b08e49adda32e2b0d51e384c80c55506d3d8f15388e73a943f360a79b173b2` |
