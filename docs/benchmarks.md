# Benchmarks

Measured on one Windows 11 machine, release build, warm cache, single run per
row. Times are wall-clock milliseconds. They are a snapshot, not a claim about
other hardware. Re-run before quoting them.

## Method

- Logs corpus: 400,000 generated JSON lines, 57.7 MB (`timestamp`, `level`,
  `service`, `route`, `status`, `latency_ms`, `user_id`, with random values).
- Source corpus: a 512 KB tar of this repository's `crates/` and `docs/`.
- gzip is the system `gzip` (`gzip -6` and `gzip -9`), which is also what
  `tar -czf` uses by default. tpt-press is the release CLI.
- Round trips were checked with `cmp` on every row that shows an unpack time.

## Results: logs corpus (57.7 MB)

| Tool and options                              | Size (bytes) | Ratio | Pack (ms) | Unpack (ms) |
|-----------------------------------------------|-------------:|------:|----------:|------------:|
| gzip -6                                       |    6,735,164 |  8.57x |       701 |         299 |
| gzip -9                                       |    6,337,785 |  9.10x |     2,241 |         296 |
| `pack` (zstd level 3, default)                |    7,637,702 |  7.55x |       330 |         112 |
| `pack --content-defined --chunk-size 65536`   |    7,586,371 |  7.61x |       237 |             |
| `pack --dictionary 110000 --chunk-size 65536` |    7,877,971 |  7.32x |       433 |             |
| `pack --level 9`                              |    6,122,073 |  9.42x |       304 |         116 |
| `pack --level 15`                             |    5,799,208 |  9.95x |       695 |         119 |
| `pack --level 19`                             |    5,089,382 | 11.34x |     2,252 |         113 |
| `pack-logs` (columnar, dictionary)            |    5,744,000 | 10.04x |     2,029 |  (`query`)  |

## Results: source corpus (512 KB)

| Tool and options                              | Size (bytes) | Ratio | Pack (ms) | Unpack (ms) |
|-----------------------------------------------|-------------:|------:|----------:|------------:|
| gzip -6                                       |      109,533 |  4.67x |        88 |          79 |
| gzip -9                                       |      108,418 |  4.72x |       114 |          80 |
| `pack` (zstd level 3, default)                |      107,357 |  4.77x |        82 |          81 |
| `pack --content-defined --chunk-size 65536`   |      121,833 |  4.20x |        78 |             |

## What the numbers say

- **Default settings trail gzip -9 on size for logs.** zstd level 3 is fast, but
  it compresses less than gzip -9 here. Level 9 beats gzip -9 on size and is
  about 7 times faster to compress. Level 19 gives the best generic ratio at
  about the same compression time as gzip -9. The default stays at 3, since
  changing it is a trade-off to make deliberately.
- **Columnar logs are the strongest result.** `pack-logs` reaches 10x at level 3,
  and a query reads only the columns it needs.
- **The dictionary does not help at this size.** A 110 KB dictionary is stored in
  the archive, and 64 KB chunks are too small for it to pay off. It is worth
  testing on larger inputs with more shared structure.
- **Content-defined chunking is neutral on a single file.** Its benefit is in
  deduplication between versions of a file, which this single-run test does not
  measure. On the 512 KB source tar it was slightly larger, because its chunks
  are smaller on average.
- Unpack is faster than gunzip on the logs corpus (112 ms against 299 ms). On the
  small source tar the two are within noise (81 ms against 79 to 80 ms).

## Not measured yet

- Real bf16 checkpoints. Only synthetic Gaussian weights are measured, above.
- Multi-version deduplication with content-defined chunking.
- Repeated runs for variance. Each row is one run. The default pack time varied
  between runs (330 ms, then 125 ms in the level sweep), so treat single-digit
  differences as noise.

## Logs: dictionary on and off (median of three runs)

Same 57.7 MB logs corpus as above.

| Command                               | Size (bytes) | Median pack (ms) |
|---------------------------------------|-------------:|-----------------:|
| `pack-logs` (dictionary, default)     |    5,744,000 |            1,980 |
| `pack-logs --no-dictionary`           |    5,333,970 |            1,291 |
| `pack` generic, level 3               |    7,637,702 |              126 |
| `pack` generic, level 9               |    6,122,073 |              367 |

On this corpus the logs dictionary makes the archive 7.7% larger and the pack
slower. A likely reason is that each 256 KB block already holds about 1,800
records, so zstd learns the repeated structure within each block without help.
That is a hypothesis, not a measured result. The default is still on, since the
dictionary may help smaller blocks or more uniform records. Turn it off with
`--no-dictionary` if it does not help your data.

## Tensors: synthetic bf16 weights

Input: 16 Mi bf16 weights (33,554,515 bytes), drawn from a Gaussian with
standard deviation 0.02 and a fixed seed. **These are synthetic weights, not a
real checkpoint,** so treat the numbers as a baseline for the float codec, not as
a claim about any model.

| Tool and options               | Size (bytes) | Saved   | Pack (ms) |
|--------------------------------|-------------:|--------:|----------:|
| gzip -6                        |   26,611,585 |   20.7% |     2,830 |
| gzip -9                        |   26,611,585 |   20.7% |     2,814 |
| `pack-tensors` (level 3)       |   26,166,909 |   22.0% |       201 |
| `pack-tensors --level 9`       |   25,889,078 |   22.8% |       629 |

Generic compressors recover about a fifth of this data, because the 7 mantissa
bits are close to random and the exponent is the only skewed part. The float
codec in `TODO.md` (section 7e) targets 25 to 35% savings by coding the exponent
and mantissa streams separately. This baseline shows the gap it needs to close,
on synthetic data.

## Tensors: a real bf16 checkpoint (SmolLM2-135M)

Input: `model.safetensors` from `HuggingFaceTB/SmolLM2-135M` on Hugging Face,
269,060,552 bytes, 272 BF16 tensors (134.5 M parameters). The same machine as
above, single run each.

| Tool and options                  | Size (bytes) | Saved | Pack (ms) |
|-----------------------------------|-------------:|------:|----------:|
| gzip -6                           |  213,332,745 | 20.7% |    22,329 |
| plain zstd, 1 MiB chunks (`pack`) |  210,439,112 | 21.8% |       691 |
| `pack-tensors` (byte shuffle, level 3) | 192,776,563 | 28.4% |  1,792 |
| `pack-tensors --level 9`          |  189,643,960 | 29.5% |     7,385 |

The byte shuffle adds about 6.6 points over plain zstd on this checkpoint, and
the same gain as on the synthetic weights above. The cost is pack time: each
bf16, f16, f32 or f64 piece is compressed twice, once plain and once shuffled,
and the smaller is kept. Unpack is unaffected by the choice, since it only has
to undo the shuffle.

The embedding tensor (56.6 MB) was read back from the archive and compared with
its byte range in the original file. The two are identical, and `inspect
--verify` reports all checksums match.

## Time series: Gorilla codecs against zstd

`crates/tpt-press-logs/examples/gorilla_sizes.rs`. One million values per series, from a
fixed generator, so the numbers repeat. Sizes in bytes. Raw is 8 bytes per value.
The Gorilla codecs are not yet part of the logs format, so this is a comparison
of codecs, not of archives.

| Series                            |      Raw |  zstd 3 | shuffle + zstd | Gorilla |
|-----------------------------------|---------:|--------:|---------------:|--------:|
| timestamps, 1 s with ±20 ms jitter | 8,000,000 |  2,046,140 |      1,176,568 | **1,100,116** |
| timestamps, every 15 s exactly     | 8,000,000 |  2,255,295 |        154,602 | **125,016** |
| temperature, read to 0.1 degree    | 8,000,000 | **661,403** |        876,936 | 3,465,652 |
| temperature, full precision        | 8,000,000 |  7,228,604 |  **6,072,513** | 6,859,959 |

What this says:

- **Timestamps:** delta-of-delta is the best of the three. An exact interval
  costs about one bit per point.
- **Quantised readings:** plain zstd is the best, and Gorilla's XOR is far
  worse (27.7 bits per value, against about 5.3 for zstd). The readings revisit
  the same few hundred values, and zstd finds those repeats. The XOR encoding
  does not, so it is the wrong choice for this kind of data.
- **Full precision:** shuffle plus zstd is the best. Gorilla beats plain zstd
  here but not the shuffle.

So no single codec wins on every series. A column-level choice would fit best:
delta-of-delta for timestamps, and, for floats, whichever of plain zstd and
shuffle-plus-zstd is smaller. That is a design decision for when these codecs
are integrated, and it is recorded in `TODO.md`.

---

The sections below were measured later, on the inputs named in each one. Each
row is a single run on the same Windows 11 machine, release build, unless it says
otherwise. Round trips were checked with `cmp`. Re-run before quoting them.

## Logs: seeded 57 MB corpus, with zip and plain zstd

Generated by `crates/tpt-press-cli/examples/make_logs_corpus.rs`, with the default seed, so
anyone can regenerate the same bytes. 400,000 JSON lines, 57,425,970 bytes.
gzip, zip and zstd are run on the same file.

| Tool and options                              | Size (bytes) | Ratio | Pack (ms) | Unpack (ms) |
|-----------------------------------------------|-------------:|------:|----------:|------------:|
| gzip -6                                       |    6,069,090 |  9.46x |       776 |         322 |
| gzip -9                                       |    5,747,676 |  9.99x |     2,167 |         313 |
| zip, deflate level 6                          |    5,827,909 |  9.85x |       779 |           - |
| zip, deflate level 9                          |    5,785,531 |  9.93x |     1,379 |           - |
| plain zstd, level 3                           |    7,540,586 |  7.62x |       121 |           - |
| plain zstd, level 19                          |    4,333,293 | 13.25x |    25,004 |           - |
| `pack` (zstd level 3)                         |    7,438,126 |  7.72x |       334 |         100 |
| `pack --level 9`                              |    5,643,473 | 10.18x |       386 |         101 |
| `pack --level 19`                             |    4,523,573 | 12.69x |     2,413 |         158 |
| `pack --content-defined --chunk-size 65536`   |    7,501,832 |  7.65x |       210 |           - |
| `pack-logs` (columnar, dictionary)            |    5,457,695 | 10.52x |     2,223 |  (`query`) |
| `pack-logs --no-dictionary`                   |    4,894,020 | 11.73x |     1,376 |  (`query`) |

`query --columns timestamp,level` over the same archive took 423 ms and returned
400,000 rows.

What this says:

- **Plain zstd at level 19 is the smallest generic result on this corpus**, ahead
  of `pack --level 19` by about 4%. The gap is the price of the 1 MB chunks and the
  frame headers that `pack` adds. It is not a tuning win for `pack`.
- **The `pack-logs` dictionary makes the archive 11% larger on this corpus**
  (5,457,695 against 4,894,020 bytes without it). That agrees with the earlier
  result, and the default stays as it is.
- **zip is a fair middle ground.** Deflate level 6 packs in about the time of
  gzip -6, and its archive is 4% smaller. zstd at level 19 is smaller again, but
  it takes about 32 times longer to pack than zip level 6.
- Timings are single runs, so treat differences under about 10% as noise.

## Genomics: E. coli K-12 MG1655 (real data)

Input: `NC_000913.3` from NCBI RefSeq, 4,699,745 bytes. One record, 4,641,652
bases, 80-column lines. The sequence has no N runs and no lowercase.

| Tool and options                       | Size (bytes) | Ratio | Pack (ms) | Unpack (ms) |
|----------------------------------------|-------------:|------:|----------:|------------:|
| `pack-genomics` (mixed context models) |    1,114,813 | 4.22x |     1,500 |       1,240 |
| earlier build: 2-bit packing only      |    1,160,813 | 4.05x |       424 |          75 |
| plain zstd, level 19                   |    1,305,697 | 3.60x |     2,605 |           - |
| plain zstd, level 3                    |    1,492,954 | 3.15x |        22 |           - |
| zip, deflate level 9                   |    1,380,253 | 3.41x |     2,488 |           - |
| zip, deflate level 6                   |    1,426,241 | 3.29x |       467 |           - |
| gzip -9                                |    1,379,130 | 3.41x |           |             |
| gzip -6                                |    1,424,003 | 3.30x |           |             |

The 2-bit row is an earlier build, from before the context models. It is kept
for comparison. The mixed archive is 46,827 bytes (4%) smaller than it. The
mixed coder codes the bases at 1.92 bits per base, against the 2-bit floor of
2.00.

**Speed.** The mixed coder runs at about 3 MB/s on this machine, against about
11 MB/s for the 2-bit path. It is the price of the smaller archive. Unpacking is
the same speed, since decoding mirrors encoding.

**Tried and removed.** Each of these was measured on E. coli, and each was
taken out again, because the gain did not justify the cost:

- *Reverse-complement learning* (train each model on the other strand after every
  base): 1,113,251 bytes, against 1,113,986 without it. A gain of 0.07%, for
  twice the table traffic. Restricting it to orders 11 and up was worse.
- *Secondary estimation* (an adaptive map from the mixer's output, indexed by
  order-4 context and node): 1,116,425 bytes with a strong blend, and 1,114,792
  with a gentle one. Neither beat the mixer alone, which gives 1,114,813.
- *Five symbols of quality context* (against three): no gain on the synthetic
  qualities, whose entropy one order-1 context already reaches.

**Random access.** `contigs.fna` is the same genome cut into 464 records of about
10 kb. The awk split gave each one 10,020 bases, from an overlap at the line
boundaries. The file is still valid input, and the round trip is exact.

| Block size | Blocks | Size (bytes) | Ratio | `fetch` one record (ms) | Blocks read |
|-----------:|-------:|-------------:|------:|------------------------:|------------:|
| 1 MiB      |      5 |    1,127,717 | 4.20x |                     492 |           1 |
| 64 KiB     |     67 |    1,157,361 | 4.09x |                      67 |           1 |

The name lookup means `fetch` reads one block, whatever the block size. What
the block size sets is how much it decodes. At 1 MiB, `fetch` decodes about
a megabyte of bases to return a 10 kb record, which takes 492 ms. At 64 KiB it
takes 67 ms, for 2.6% more archive. Choose the block size by which matters more
for the data.

**Not measured:** bgzip and CRAM (neither is installed here), and any genome with N
runs or soft-masked lowercase. Those paths are covered by tests, not benchmarks.

## Genomics: synthetic FASTQ, quality scores

**This is synthetic data.** 60,000 reads of 100 bases, taken from random
positions of the E. coli genome. Qualities come from a generated process: a
random walk with a slight downward drift, and occasional sudden drops. No real
sequencer output is available here, so these numbers say how the quality coder
behaves on this process. They do not say how it does on real reads. Input:
13,008,890 bytes.

| Tool and options                            | Size (bytes) | Ratio |
|---------------------------------------------|-------------:|------:|
| `pack-genomics` (whole FASTQ)               |    3,907,757 | 3.33x |
| `pack-genomics` with constant qualities     |    2,109,914 | 6.17x |
| plain zstd, level 19 (whole FASTQ)          |    3,925,082 | 3.31x |
| zip, deflate level 9 (whole FASTQ)          |    5,103,376 | 2.55x |
| plain zstd, level 19 (qualities alone)      |    2,317,111 |       |

The quality cost is the difference between the first two rows, 1,797,843 bytes.
An empirical order-1 entropy of this process is 1.78 MB, so the coder is about
1% above the entropy of the process. zstd -19 spends 2.32 MB on the same
qualities, about 29% more. The archive is only slightly smaller than zstd on the
whole file, because the bases and identifiers cost about the same in both.

## Arrays: NumPy float32 field (synthetic)

Input: a 2000 x 1500 `float32` field, `20 + 5 sin(x/97) + 3 cos(y/131)`, saved
with `numpy.save` (NumPy 2.2.6). It is smooth, so it is a best case for the
delta filters, and it is **not a real dataset**. Input: 12,000,128 bytes.

| Tool and options                       | Size (bytes) | Ratio   | Pack (ms) | Unpack (ms) |
|----------------------------------------|-------------:|--------:|----------:|------------:|
| `pack-arrays` (auto filters)           |      116,378 | 103.11x |       312 |          95 |
| `pack-arrays`, xor+shuffle only (v1)   |    4,110,682 |   2.92x |       183 |         100 |
| plain zstd, level 19                   |    9,657,036 |   1.24x |     3,190 |           - |
| plain zstd, level 3                    |   10,003,755 |   1.20x |        25 |           - |
| zip, deflate level 6                   |   10,018,170 |   1.20x |       496 |           - |
| gzip -9                                |   10,006,499 |   1.20x |           |             |

All 12 chunks chose delta+shuffle. Subtracting the previous float's bit pattern
leaves small residuals, because neighbouring values share most bits, and the
shuffle groups their zero bytes. The XOR filter, which was the v1 choice, gets
2.92x. A row-wise filter does worse on this field, and auto never picks it here.
Unpacking the `.npy` archive gives a file identical to the original.

A second synthetic input, an int16 image made by a random walk along both axes
(600 x 800), chose the row-wise delta, which the XOR filter did not beat:

| Input                                  | Size (bytes) | Ratio | Chosen filter |
|----------------------------------------|-------------:|------:|---------------|
| random walk image, int16 (`pack-arrays`) |      329,590 | 2.91x | lorenzo+shuffle |
| same, delta-up only (earlier build)      |      441,007 | 2.18x | delta-up+shuffle |
| same, earlier build (XOR filters only) |    501,834 | 1.91x | shuffle       |
| uniform random int16 (-300 to 300)     |      130,135 | 1.54x | shuffle       |

Uniform random data gives 1.54x, and its entropy allows about 1.7x.

Other checks, all on the same field:

- `slice-arrays --range 100:110 --range 200:260` matched `arr[100:110, 200:260]`
  as read by NumPy, and decoded one chunk.

**Not measured:** Blosc and Zarr (not installed here), and real climate, image or
simulation data. Those are the inputs where the filters would be tested for real.
Auto now tries seven filters per chunk, so pack time grows with that count. On the
2000 x 1500 field it packs in about 0.7 s, against about 0.2 s with one filter.

## Exponent split and timestamp encoding (single run each)

- bf16 synthetic Gaussian weights, 1 Mi values (2 MiB), `pack-tensors`
  defaults. Byte shuffle only: 1,489,317 bytes (71.0%). With the exponent split:
  1,471,589 bytes (70.2%). The split is a small gain here. Real checkpoints are
  not measured yet.
- Logs, 200,000 generated JSON lines (19.8 MB, timestamps 1 to 3 s apart,
  random level, service, status and latency). Before the Gorilla timestamp
  column: 1,202,174 bytes (16.5x). After: 973,487 bytes (20.4x), 19% smaller.
  A full `query` returned every timestamp and level identical to the input.

## Dataset archive vs tar shard (single run each)

`crates/tpt-press-datasets/examples/random_access.rs`. 20,000 samples, each a
4 KiB random "image" and a ~250-byte JSON record. 86.2 MB of member bytes.

- Tar shard, WebDataset layout: 112,641,024 bytes (130.6% of the member bytes).
  Each file costs a 512-byte header, plus padding.
- Dataset archive: 83,807,921 bytes (97.2%).
- 5,000 random sample reads. Tar with an offset table built from its headers:
  4.9 to 8.9 ms across two runs. Dataset: 7.5 to 18.4 ms across the same runs.

The comparison is not like for like, and the numbers are noisy between runs. The
tar side reads the raw bytes at the offsets and does no checksum or
decompression. The dataset side reads each member, checks its CRC32, and
decompresses zstd members. The size result is the stronger one.

## Delta against a base model (synthetic, single run)

Base: 1 Mi bf16 values (2 MiB), Gaussian-like with a fixed seed. Fine-tune: the
same values, with the two lowest mantissa bits changed on about one value in
four, pseudo-random. This is an assumption about fine-tunes, not a measured
one. Real fine-tunes are not measured yet.

- Fine-tune packed alone: 735,051 bytes of 1,052,672 raw (including a small
  extra tensor).
- Fine-tune packed with `--base`: 137,024 bytes, 18.6% of the standalone size.
- The round trip is exact, with and without `verify`.

A regular change, the same bit on every eighth value, collapses to a few hundred
bytes. That is an artifact of the pattern, so it is not reported as a result.

## fp16 field split: tried and not kept (synthetic, single run)

Tried a field split for fp16: the sign and exponent in one plane, and the
mantissa in its low and high bytes, each compressed with zstd. Measured on 1 Mi
synthetic fp16 values (skewed exponents near 1.0, random sign and mantissa):

- shuffle plus zstd: 1,803,501 bytes
- field split: 1,906,064 bytes (5.7% larger)

The split lost, so the writer would never have chosen it. It was removed, not
kept as a dead format code. The random mantissa dominates the size, and the
shuffle already keeps the exponent bits together. Real fp16 weights are not
measured, so this is a result for this synthetic data only.

## VCF (synthetic, single run)

200,000 generated records, 16.2 MB: random chromosome positions, random alleles,
random sample genotypes over four samples. Random genotypes are close to the
worst case for a VCF, so real files should do better.

- gzip -9: 3,736,325 bytes
- tpt-press genomics (VCF): 2,697,671 bytes (28% smaller than gzip -9)
- The round trip is byte-identical (`cmp`).

Small files grow. A two-record file takes 632 bytes, because each block has a
fixed cost.
