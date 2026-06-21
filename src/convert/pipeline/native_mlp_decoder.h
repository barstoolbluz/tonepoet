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

#ifdef __cplusplus
}
#endif

#endif
