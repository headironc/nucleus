# 字体测试夹具

仅用于字体净化和固定工牌排版测试，不是生产字体或正式打印依据。

- `SourceSans3-Regular.ttf`：431196 字节，SHA-256 `4644c81b86ec9caaa76b634889968ed3c4f4f52f054855933acc7c2b21e53b0f`。
- `SourceSans3-Regular.otf`：334924 字节，SHA-256 `08df266400933d3178d081a45f94a08814c3e55b4b7dd2e0ff69cb1329f13ab6`。
- `LICENSE.md`：原许可文本，SHA-256 `56af9b9c6715597e458284a474dc118a50a4150e9d547c70f7b4a33c3e6a9328`。

来源：Adobe Source Sans 3，提交 `87b37a2daaed80fcb8e8ccb0085c4d72ddade12e` 的 `TTF/SourceSans3-Regular.ttf` 与 `OTF/SourceSans3-Regular.otf`，<https://github.com/adobe-fonts/source-sans/tree/87b37a2daaed80fcb8e8ccb0085c4d72ddade12e>。许可为 SIL Open Font License 1.1，保留字体名 Source，见 `LICENSE.md`。

这些文件保持原始字节不变。测试只在内存中改动副本，不在此保存修改后的字体文件。

工牌排版还使用两份原始 Noto 字体，验证中文换行、阿拉伯文连接和混合方向。测试不把字形覆盖当作生产许可或实际打印验收。

| 文件                     | SHA-256                                                            | 固定来源                                                                                                                                                            |
| ------------------------ | ------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `NotoSansSC-Regular.otf` | `faa6c9df652116dde789d351359f3d7e5d2285a2b2a1f04a2d7244df706d5ea9` | [Noto CJK](https://github.com/notofonts/noto-cjk/blob/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/SubsetOTF/SC/NotoSansSC-Regular.otf)                            |
| `NotoSansArabic.ttf`     | `63111b5b2e074dd48cc67692e0a2726d86ee94c1c37fe8598257b7b4e87e869e` | [Google Fonts Noto Sans Arabic](https://github.com/google/fonts/blob/6c1dbe27dcf7d817d4b62cec033ba77fa8e2a5ec/ofl/notosansarabic/NotoSansArabic%5Bwdth,wght%5D.ttf) |

两者均按 SIL Open Font License 1.1 保留原许可，分别见 `NotoSansSC-LICENSE.txt` 和 `NotoSansArabic-OFL.txt`。阿拉伯字体保留上游原名，仅以本地文件名去掉轴列表。没有改动字体字节。
