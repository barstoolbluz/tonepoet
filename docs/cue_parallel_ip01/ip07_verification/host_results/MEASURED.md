# IP-07 host measurement, 2026-10-25

Reference album, twelve tracks, 45 minutes, 16-bit 44.1 kHz stereo FLAC+CUE to FLAC at
source rate and depth. Local NVMe, 32-core host, three repetitions, medians.

| route | ReplayGain Off | ReplayGain Both |
| --- | --- | --- |
| S1 | 15.04 s | 18.18 s |
| S3 pilot | 10.28 s | 10.81 s |

Pilot against S1 under the default: **1.68x**.

Cost of ReplayGain, same route both columns: S1 pays 3.14 s, the pilot pays **0.53 s**.
Before IP-07 the pilot paid 12.60 s for a post-output scan and finished at 23.21 s.

Correctness: against S1, in both ReplayGain modes, all twelve decoded audio MD5s and
every tag are identical, not only the ReplayGain ones. Album gain -0.36 dB, album peak
0.928986 on both routes.

Destination: file set identical to S1's in both modes, and the delivered
`verify_destination_files.py` gate reports 13 requested files with no hidden or
unrequested state.
