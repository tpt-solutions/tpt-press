# Literature check: lossless model-weight compression

Checked before building the float codec (TODO section 7e). Sources are
abstracts and early method sections, not full reproductions. Numbers below are
the papers' own claims unless marked otherwise.

## Sources

- ZipNN: Hershcovitch et al., "ZipNN: Lossless Compression for AI Models",
  [arXiv 2411.05239](https://arxiv.org/pdf/2411.05239).
- DFloat11: "70% Size, 100% Accuracy: Lossless LLM Compression for Efficient
  GPU Inference via Dynamic-Length Float", [arXiv 2504.11651](https://arxiv.org/pdf/2504.11651).
- Heilper and Singer, "Lossless Compression of Neural Network Components:
  Weights, Checkpoints, and K/V Caches in Low-Precision Formats",
  [arXiv 2508.19263](https://arxiv.org/abs/2508.19263v1).
- ZipNN reference implementation on [PyPI](https://pypi.org/project/zipnn/0.2.1)
  (vendor benchmark, not independently checked).

## Findings

1. **The exponent carries the savings.** In BF16 the exponent is 8 of 16 bits.
   It is highly skewed, and the skew is in single bytes, so Huffman coding of
   the exponent stream beats Lempel-Ziv. ZipNN reports Huffman-only coding as
   both smaller and faster than its earlier byte-grouping plus general
   compressor pipeline.
2. **Size results.** ZipNN: about 33% saved on popular BF16 models, over 50% on
   some "clean" ones. DFloat11: about 30% saved on BF16 LLMs, bit-identical
   output, with a GPU decoder. Heilper and Singer: up to 62% saved on BF16 and
   83% on FP8 (their FP8 figure is for the FP8 representation, not comparable
   to our BF16 number).
3. **Already-quantized checkpoints compress much less.** The 2508.19263 paper
   reports that GGUF quantized checkpoints do not shrink at all, and other
   quantized ones only reach about 85 to 91% of their size.
4. **Our measured baseline:** byte-plane shuffle plus zstd saves 28% on
   SmolLM2-135M (`docs/benchmarks.md`), already above plain zstd's 22%.

## What this means for the plan

- The target in TODO 7e (25 to 35% saved with an exponent/mantissa split and
  entropy coding) matches the published range. It is a reasonable target.
- Exponent Huffman with a separate mantissa stream is the design to build.
  Entropy coding the exponent alone, not the whole byte, is what the papers
  use. ANS is not required; Huffman is what ZipNN chose for speed.
- Expect less gain on GGUF and other pre-quantized inputs. The codec should
  keep the raw fallback, as planned.
- Our measured numbers come from one model. The published ones come from
  several. Treat the 30% figure as a hope until we measure a second real
  checkpoint.

## How the split is implemented

`docs/literature.md` describes the published method, which uses Huffman coding
on the exponent. Our split (`crates/tpt-press-tensors/src/split.rs`) puts the
exponent in its own byte plane and compresses it with zstd. zstd entropy-codes
literal bytes with Huffman, and a plane with no repeated multi-byte strings is
exactly the case where that works. So this is Huffman coding of the exponent,
through zstd, with no separate coder to maintain.
