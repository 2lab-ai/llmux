# macOS and KDE visual receipts

These PNGs are durable, reviewable evidence for the native macOS shell and the
Arch Linux/KDE port. They were captured from the production renderers with the
same deterministic, privacy-masked dashboard fixture at source commit
[`0857d5839edee1fa7af5b2034bd1d41d2e6e1f87`](https://github.com/2lab-ai/llmux/commit/0857d5839edee1fa7af5b2034bd1d41d2e6e1f87)
by [Islands parity run 29374043773](https://github.com/2lab-ai/llmux/actions/runs/29374043773).
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
| KDE | Usage, full surface | `kde-usage-full.png` | 960x760 | `7407b3dfa684776d8a107020e18b1fef3bbd828f7bb629ce8c30dbb5e91c4e3e` |
| KDE | Statistics, full surface | `kde-statistics-full.png` | 960x1118 | `075495e5a9438ec8b7c7a502609fdcf0e435589999c2d675110a6fe946aa8e24` |
| KDE | Request and verification receipts | `kde-receipts-detail.png` | 924x363 | `4262d4ada0f6ba88c1400de9c97ecb5c73917ca6d0a967c7139b4d01156f6895` |
| KDE | Menu, full surface | `kde-menu-full.png` | 960x1409 | `5406d2e9adc0b0e5c9d4effa35ef1b241eaa5e31137976c3613dd736731cfb88` |
