# Convert de-emphasis probe fixture

`preemphasis_16_44100.flac` is a metadata-only derivative of
`tests/fixtures/silence.flac`. Its encoded audio MD5 is unchanged
(`a9e16d2dd82c6ebc69f119687f707372`); only the Vorbis comment
`PRE_EMPHASIS=1` was added.

Recreate it from the repository root with:

```bash
cp tests/fixtures/silence.flac tests/fixtures/deemphasis/preemphasis_16_44100.flac
metaflac --remove-tag=PRE_EMPHASIS --set-tag=PRE_EMPHASIS=1 \
  tests/fixtures/deemphasis/preemphasis_16_44100.flac
```

The fixture is 16-bit integer stereo PCM at 44.1 kHz inside FLAC, so it is a
minimal on-disk source for the Convert CD de-emphasis eligibility regression.
