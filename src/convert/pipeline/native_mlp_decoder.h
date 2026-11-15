#ifndef TONEPOET_NATIVE_MLP_DECODER_H
#define TONEPOET_NATIVE_MLP_DECODER_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define TONEPOET_NATIVE_MLP_TEXT_CAP 1024

struct tonepoet_native_mlp_decoder_info {
    int decoder_available;
    int downmix_option_available;
    int private_downmix_layout_available;
    int private_downmix_layout_set;
    int downmix_option_offset;
    int private_downmix_layout_offset;
    unsigned int avcodec_version;
    char avcodec_version_text[TONEPOET_NATIVE_MLP_TEXT_CAP];
    char avcodec_configuration[TONEPOET_NATIVE_MLP_TEXT_CAP];
    char error[TONEPOET_NATIVE_MLP_TEXT_CAP];
};

struct tonepoet_native_mlp_decode_result {
    int channels;
    int sample_rate;
    uint64_t samples_per_channel;
    uint64_t data_bytes;
    unsigned int avcodec_version;
    int private_downmix_layout_set;
    int downmix_option_offset;
    int private_downmix_layout_offset;
    char channel_layout[TONEPOET_NATIVE_MLP_TEXT_CAP];
    char error[TONEPOET_NATIVE_MLP_TEXT_CAP];
};

int tonepoet_native_mlp_decoder_info(struct tonepoet_native_mlp_decoder_info *out);

int tonepoet_native_mlp_decode_stereo_s32le_wav(
    const char *input_path,
    const char *output_path,
    struct tonepoet_native_mlp_decode_result *out);

/* Extraction-only fixed RF64 carrier; the Convert ABI remains unchanged. */
int tonepoet_native_mlp_decode_stereo_s32le_rf64(
    const char *input_path,
    const char *output_path,
    struct tonepoet_native_mlp_decode_result *out);

/* Extraction-only hook, invoked once per bounded MLP input chunk. A
 * nonzero return aborts decode and removes the private incomplete WAV. */
typedef int (*tonepoet_native_mlp_checkpoint)(void *context);
int tonepoet_native_mlp_decode_stereo_s32le_controlled(
    const char *input_path,
    const char *output_path,
    struct tonepoet_native_mlp_decode_result *out,
    int native_rf64,
    tonepoet_native_mlp_checkpoint checkpoint,
    void *checkpoint_context);

#ifdef __cplusplus
}
#endif

#endif
