# Cold-source measurement over sshfs to a spinning disk

2026-10-25. Addresses the gap left by the NVMe matrix: whether independent range readers
are punished by a seeking source, which the original analysis named as a reason to
withhold auto-admission.

**Source:** `daedalus@192.168.0.43:/mnt/rosebud/archive/torrents/` over `fuse.sshfs`,
backed by a spinning disk. Destination and staging on local NVMe. This is the user's real
listening library, not a synthetic fixture.

Page cache cannot be dropped without root here, so cold reads were obtained by first
touching albums not read in this session. Each album was then re-run on the other route
and again on the first route, both warm. Which route touched cold alternates by album, so
route is not confounded with album.

| album | tracks | cold-first route | cold | warm, same route | cold penalty | other route, warm | speedup |
| --- | --- | --- | --- | --- | --- | --- | --- |
| McCoy Tyner, A Night of Ballads & Blues | 8 | S1 | 14.68 s | 14.62 s | +0.06 s | S3 8.73 s | 1.67x |
| Sarah Vaughan, Swingin' Easy | 13 | S3 | 10.62 s | 10.75 s | -0.13 s | S1 17.00 s | 1.58x |
| McCoy Tyner, Reaching Fourth | 6 | S1 | 12.62 s | 12.70 s | -0.08 s | S3 7.68 s | 1.65x |
| The Dave Brubeck Quartet, Time Out | 7 | S3 | 8.98 s | 8.79 s | +0.19 s | S1 14.19 s | 1.61x |

ReplayGain Both throughout, the Convert UI default.

**Neither route shows a measurable cold penalty, and S3 shows no disproportionate one.**
Both sit inside the run-to-run spread seen on NVMe. S3 is faster on every album, 1.58x to
1.67x, consistent with the NVMe matrix.

The reason is visible in the earlier decomposition: decoding the whole image is about a
second of work, overlapped against seconds of encoding. The source read was never the
bottleneck, so making it concurrent neither helps nor hurts much.

## Limits of this result

sshfs readahead and the remote host's own page cache sit between the measurement and the
platter, so this does not isolate raw seek behaviour on a directly attached drive. It
measures the configuration the user actually converts from.

Four albums, one network, one remote host. A directly attached spinning disk under memory
pressure remains unmeasured.
