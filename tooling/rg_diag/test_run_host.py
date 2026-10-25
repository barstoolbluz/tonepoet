#!/usr/bin/env python3
"""Known S1 command-role classifier and privacy regression tests."""
import unittest
import run_host


class ProcRoleTests(unittest.TestCase):
    def test_root_cpu_is_not_claimed_as_exclusive_replaygain(self):
        self.assertEqual(run_host.classify_process_role("tonepoet", [], True),
                         "tonepoet_app_including_native_meter")

    def test_non_tonepoet_root_is_unclassified(self):
        self.assertEqual(run_host.classify_process_role("python3", [], True),
                         "root_application_unclassified")

    def test_cue_s1_ffmpeg_decode(self):
        argv = ["ffmpeg", "-nostdin", "-i", "/secret/album.wav", "-f", "s32le",
                "-c:a", "pcm_s32le", "pipe:1"]
        self.assertEqual(run_host.classify_process_role("ffmpeg", argv), "cue_stream_decoder")

    def test_cue_s1_ffmpeg_encode(self):
        argv = ["ffmpeg", "-nostdin", "-f", "s32le", "-i", "pipe:0",
                "-c:a", "flac", "/secret/track.flac"]
        self.assertEqual(run_host.classify_process_role("ffmpeg", argv), "cue_stream_encoder")

    def test_cue_s1_sox_encode(self):
        argv = ["sox", "-S", "-D", "-t", "raw", "-", "/secret/track.wav"]
        self.assertEqual(run_host.classify_process_role("sox", argv), "cue_stream_encoder")

    def test_generic_ffmpeg_never_guessed_as_s1_role(self):
        argv = ["ffmpeg", "-i", "/secret/album.wav", "/secret/track.wav"]
        self.assertEqual(run_host.classify_process_role("ffmpeg", argv),
                         "other_ffmpeg_or_unclassified")


if __name__ == "__main__":
    unittest.main()
