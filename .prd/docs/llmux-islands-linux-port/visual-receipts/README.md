# macOS and KDE visual receipts

These 14 PNGs are durable, reviewable evidence for the renewed native macOS
shell and Arch Linux/KDE port. They were captured from the production
renderers with byte-identical, deterministic, privacy-masked dashboard
fixtures by [Islands parity run 29391046261](https://github.com/2lab-ai/llmux/actions/runs/29391046261)
from source tree
[`9e0d5a388559a1ad7ef4b48c6827d68591e67be4`](https://github.com/2lab-ai/llmux/commit/9e0d5a388559a1ad7ef4b48c6827d68591e67be4).
That Islands source was replayed content-identically as
[`1365ec68992c16bb0014c325dbe0971b2357c88d`](https://github.com/2lab-ai/llmux/commit/1365ec68992c16bb0014c325dbe0971b2357c88d)
on current master `abd68fb671b5b093480adc11c7ab7a1096ecf873`.
The run Xcode-tested the macOS app and built, packaged, installed, and
smoke-tested the KDE shell in a clean Arch environment before publishing the
`llmux-islands-macos-snapshots` and `llmux-islands-kde-snapshots` artifacts.

Both renderers use the same masked dashboard document. Account email addresses
are anonymized, API keys and request/response bodies are excluded, and unsafe
note content is rendered as `[REDACTED]`. The receipt captures expose only
request metadata and a successful settings-mutation daemon-readback receipt.

| Platform | Surface | File | Dimensions | SHA-256 |
|---|---|---|---:|---|
| macOS | Usage, default | `macos-usage-full.png` | 1120x680 | `9739bba9d09f2f5877e24127b810631e169f99210785e9853274b232b86ecb46` |
| macOS | Usage, Advanced | `macos-usage-advanced-full.png` | 1120x1240 | `625ac6f684813c09f4af3ab8b4ba00e74f3f0f66294aadc717c3abf198e1629f` |
| macOS | Statistics, default | `macos-statistics-full.png` | 1120x960 | `889142cf302007cd4bd56d6398c940e037e46e045116e3264987d348d76b5eb4` |
| macOS | Statistics, Advanced | `macos-statistics-advanced-full.png` | 1120x1720 | `b205b717de1ff657b07d911f1805500b8c06dee3e5cc0df1fafb3d9cdb711c6a` |
| macOS | Request and verification receipts | `macos-receipts-detail.png` | 1120x1400 | `cc262a790827dfc7a2d4a7e9b8c5dd5ae704959a9b4f77368b0d1c10498a3763` |
| macOS | Settings, default | `macos-menu-full.png` | 1000x920 | `0b6b93615241d2b8589ec69b860bc5086e50dfe47960d24780b894f06b247106` |
| macOS | Settings, Advanced | `macos-menu-advanced-full.png` | 1000x1640 | `f38f08c2c111395dcb03674b83587ebb7243824c61a2ad63b979d37ae3834b6d` |
| KDE | Usage, default | `kde-usage-full.png` | 960x760 | `38404e8aabb60ef3a3b652c82c99a1e8c729051dec2ed115d80ecb39348195fb` |
| KDE | Usage, Advanced | `kde-usage-advanced-full.png` | 960x760 | `7059291c07e9583fd239ca974798211579a6f242f1d593fedd9cecd6434468c7` |
| KDE | Statistics, default | `kde-statistics-full.png` | 960x760 | `68588c52ff8c7aa1419f3ad098df5ff80ce84b52ca7c8dd660ced99f3026914a` |
| KDE | Statistics, Advanced | `kde-statistics-advanced-full.png` | 960x930 | `32c29c2b17f85ecc7f960abc7de6752963e038b2925b895fa2beb853c8889267` |
| KDE | Request and verification receipts | `kde-receipts-detail.png` | 960x967 | `009d224e97b2851d6867d4123de58c251d76025d66c40d0e9b2bf47f13bda2ef` |
| KDE | Settings, default | `kde-menu-full.png` | 960x760 | `a9377d1ebc499b05b45aebadd5ab1b50833bbac3db71069c32d47387ec39d403` |
| KDE | Settings, Advanced | `kde-menu-advanced-full.png` | 960x1512 | `88cf87662537aea76bfc151c8279e34902ecf755d8400509c9f00b554e6a0181` |

All 14 files were checked against their CI artifact manifests. Each platform's
seven PNGs have distinct SHA-256 values, and every file was inspected at original
resolution for complete shell chrome, content bounds, default/Advanced
hierarchy, receipt visibility, masking, and secret exclusion.
